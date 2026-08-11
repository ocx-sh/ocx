# ADR: Lazy Package Loading — Declared Executables Materialize on First Exec

## Metadata

**Status:** Proposed
**Date:** 2026-08-09
**Deciders:** Owner (design session recorded as [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302)) + architect (this ADR: verification, code-grounded corrections, Windows blocker)
**GitHub Issues:** [ocx-sh/ocx#301](https://github.com/ocx-sh/ocx/issues/301) (hardlink the Windows shim blob), [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302) (lazy package loading) — [#302](https://github.com/ocx-sh/ocx/issues/302) depends on [#301](https://github.com/ocx-sh/ocx/issues/301); implemented as one plan
**Tech Strategy Alignment:**
- [x] Rust 2024 / Tokio / no new dependency — Golden Path (`product-tech-strategy.md`)
**Domain Tags:** package-manager, file-structure, cli, windows, devops
**Reversibility class:** Two-Way Door — Medium. Default is `never`; one new local-only store; no wire format, no OCI manifest, no metadata field, no lock schema. Two durable commitments: the shim-store path layout (derived state, regenerable from `ocx.lock`) and — on Windows only — one new launcher sidecar (§D2).

**Relationship to [#302](https://github.com/ocx-sh/ocx/issues/302).** [#302](https://github.com/ocx-sh/ocx/issues/302) is the design record and this ADR does not redesign it. What follows is the decision layer around it: the settled decisions recorded as accepted context (§Accepted Context), four options weighted so the chosen shape is defensible rather than merely inherited (§Considered Options), and the places where reading the code changed something (§Corrections). Three of the calling brief's Discover findings turned out to be aimed at the brief's *summary* of [#302](https://github.com/ocx-sh/ocx/issues/302) rather than at [#302](https://github.com/ocx-sh/ocx/issues/302) itself; those are withdrawn explicitly rather than silently dropped (§Withdrawn).

---

## Context

Entering a project materializes every tool in the lock, including the ones the
session never invokes. A JDK + CMake + Python toolchain costs hundreds of MB
before the first command runs.

`Bundle.binaries` (`adr_declared_binaries_metadata.md`) already declares, per
package, which executable names reach the interface `PATH` surface — exactly what
is needed to put a name on `PATH` without materializing the package behind it. It
also puts OCX in the **declared-name** family (aqua, Volta, Hermit) rather than the
scanned-name family (mise, asdf), so OCX does not hit the bootstrapping paradox
those tools document (`research_lazy_shim_prior_art.md` trap 2).

[#301](https://github.com/ocx-sh/ocx/issues/301) is the storage half:
`launcher::generate` writes the embedded shim blob as an independent copy per
entrypoint (`generate.rs:87-93`) — 235 KB (aarch64) / 329 KB (x86_64) each. Laziness
multiplies the count of on-disk shim binaries by the declared surface, so the
per-file copy becomes the dominant cost of a prepared-but-unmaterialized toolchain.

## Decision Drivers

- **D-a — laziness defers *content*, never *resolution*.** Resolution stays eager and
  digest-pinned; only layer download and content assembly move to first exec. A tool
  that materializes late must be byte-identical to one materialized early.
- **D-b — reuse the composition and materialization machinery, do not fork it.**
  `find_or_install` already materializes-on-miss including dependencies, already
  composes the interface view, already refuses under `--offline`.
- **D-c — concurrency is solved; do not re-solve it.** First-exec races are the
  repeatedly-hit, partly-open failure class industry-wide (rustup
  [#988](https://github.com/rust-lang/rustup/issues/988) open; pyenv; uv
  [#15335](https://github.com/astral-sh/uv/issues/15335)). OCX's pull already has a
  three-layer cross-process defense (`pull.rs:95-116`).
- **D-d — a PATH shim has a hard structural ceiling.** It cannot serve path
  *dereference* consumers (`$JAVA_HOME/bin/java`, `pkg-config`, CMake
  `find_package`, macOS SIP stripping `DYLD_*`). Architectural boundary, not an
  implementation gap (`research_lazy_shim_robustness.md` constraint 2).
- **D-e — a shim runs inside someone else's process tree.** Its stdout, stderr and
  exit code belong to the caller, not to ocx.
- **D-f — eligibility is declared, never inferred.** A classifier makes laziness an
  emergent property of someone else's metadata.

## Industry Context & Research

**Research artifacts:** [`research_lazy_shim_prior_art.md`](./research_lazy_shim_prior_art.md),
[`research_lazy_shim_robustness.md`](./research_lazy_shim_robustness.md),
[`research_lazy_digest_fetch_and_gc.md`](./research_lazy_digest_fetch_and_gc.md).

**Trending approach:** every surveyed tool converged on one shared proxy binary with
N links to it, and every one that started with script stubs rewrote them native
(aqua v2.30, proto v0.26, Volta, mise). OCX already shipped that endpoint for
entrypoints (`adr_windows_exe_shim.md`).

**Where [#302](https://github.com/ocx-sh/ocx/issues/302) beats the field.** Every
surveyed tool leaves its shim permanently in the resolution path and pays a per-exec
tax forever — the asdf 120–150 ms lesson `adr_project_toolchain_links.md` cites
against its Option E. [#302](https://github.com/ocx-sh/ocx/issues/302)'s
PATH-shadowing model (S7) makes the indirection *transient*: steady-state exec cost
is zero. No surveyed tool does this.

**Counter-insight, worth stating once:** deferral moves registry cost in time; it
does not reduce it. Every digest GET still costs one authenticated request and one
rate-limit unit (`research_lazy_digest_fetch_and_gc.md` §1, `client.rs:652-655`).
The win is never fetching what is never used.

---

## Accepted Context — settled in [#302](https://github.com/ocx-sh/ocx/issues/302), not re-opened here

| # | Settled decision |
|---|---|
| S1 | **Unit of laziness = the tool entry** (the `ocx.toml` / `ocx.lock` binding), never the individual node. Triggering a shim materializes that entry via the ordinary pull, which walks its own dependency closure. No recursive laziness. |
| S2 | **Laziness requires a fetchable source.** A package materialized from a local bundle (`ocx package test`, `ocx patch test`, `pull_local --dest-override`) has no remote to fetch from and can never be lazy. |
| S3 | **Eligibility is declared, plus an advisory.** One hard gate: names must be enumerable (`binaries` and/or `entrypoints` declared) or refuse, naming the missing claim. A non-refusing advisory with typed reasons (`InstallPathRootedNonPathVar`, `UndeclaredBinaries`, `CombinedPathValue`) warns at lock/compose time, surfaced in `--format json`. One shared detector, so the warning can never disagree with a later named strategy. |
| S4 | **`lazy-mode` is a strategy enum** (`never` \| `always`, open to future named strategies), not a boolean. Precedence, most specific first: `--lazy-mode ▸ [package."<id>"] ▸ [group.<g>] ▸ toolchain ▸ OCX_LAZY_MODE ▸ never`. Each level `Option<LazyMode>`; `OCX_LAZY_MODE` is a **default, not an override**. No `config.toml` tier, no enforcement tier, no per-binding `[tools]` tier. |
| S5 | **Separate store**, keyed by **identity** (registry + repo + digest) — `$OCX_HOME/shims/<registry>/<repo-slug>/<algo>/<2hex>/<30hex>/`, holding the launchers plus `refs/blobs/` forward-refs into `$OCX_HOME/blobs`. A package dir is keyed by **content** (repository dropped for dedup). Different keys ⇒ different stores. Generation is temp-dir + atomic rename, so existence means complete. **Shim dir present + package dir absent = deferred** — structural state detection, no field, no flag. |
| S6 | **`ocx launcher shim '<pinned-id>' -- "$(basename "$0")" "$@"`** — a **sibling verb**, not `launcher exec`. The shim composes **this package's interface view** and resolves the name there: one path, no branching on whether the name is a binary or an entrypoint. PATH order inside that view does the dispatch. |
| S7 | **PATH shadowing, not a permanent indirection.** For a deferred tool all three directories are emitted; push order `shims/ → ${installPath}/bin → entrypoints/`, so resolution is `entrypoints/ > bin/ > shims/`. Nonexistent directories are skipped by PATH lookup, so the shim answers while content is absent and is silently shadowed the moment the real directories appear — in an already-exported shell, no re-export, **no self-replacing file** (which Windows forbids). Steady-state exec cost is **zero**. |
| S8 | **Shims are emitted regardless of cache state.** The composed env is a pure function of `(lock, lazy-mode)`; cache state changes nothing observable. `shims/` carries INTERFACE visibility like `entrypoints/` — absent under `--self`, so the private view never routes through shims. |
| S9 | **Posture:** `--frozen` → pull allowed (pinned digest, no discovery, no pointer moves); `--offline` → refused, exit 81, at exec time. Materialization is **by digest**: no tag resolution, nothing written under `index/`. A shim's resolution posture equals a plain `ocx` invocation in the shell that fired it; under `ocx run` / launcher re-entry it arrives via `apply_ocx_config`. |
| S10 | **GC — two independent jobs, neither referencing the other.** Shim liveness = its (repo, digest) is in the lock-pinned root set `collect_project_roots` already computes. Blob retention = `shims/…/refs/blobs/`, the existing forward-ref pattern. A shim deliberately **outlives** its package's content: after `ocx clean` frees disk the toolchain stays invocable and re-fetches on next use. `ocx package install --lazy-mode=always` points the candidate symlink at the package-dir path, which dangles until materialization — no repoint, no back-ref. |
| S11 | **`lazy-report`** (`silent` default \| `progress`) writes to the **controlling terminal** (`/dev/tty`, `CONOUT$`), never inherited fd 2 — a shim runs inside somebody's `$(...)` capture or CI log parser. Errors always go to stderr, outside the setting. |
| S12 | **`ocx package which`** takes the policy, **never triggers materialization**, reports `kind: binary \| entrypoint \| shim` plus the owning tool entry, per [#302](https://github.com/ocx-sh/ocx/issues/302)'s four-row table. `--no-pull` composes without a special case: `--no-pull` decides whether ocx may create anything, `lazy-mode` decides what creating means. |
| S13 | **[#301](https://github.com/ocx-sh/ocx/issues/301):** content-addressed `$OCX_HOME/.bin/ocx-shim/<sha256>.exe`, written once, hardlinked per launcher; `SHIM_SHA256` is a compile-time constant so the hot path is a `stat`. Content-addressed rather than fixed-path so several ocx versions share one `$OCX_HOME` without last-writer-wins. Unix launchers are per-package shell scripts, unaffected. No migration: existing copies keep working. |
| S14 | **Non-goals (v1):** transitive / per-dependency laziness; `lazy-mode = "auto"`; an enforcement tier; a per-binding tier inside `[tools]`; relocatable `$OCX_HOME`. |

---

## Considered Options

Weighed so the chosen shape is defensible rather than inherited. Option B is
[#302](https://github.com/ocx-sh/ocx/issues/302)'s and it wins. Option D is the
cheaper alternative I built independently before reading
[#302](https://github.com/ocx-sh/ocx/issues/302), and it is **wrong on a correctness
axis** — recorded because "why not just reuse `launcher exec`?" is the first question
a reviewer will ask, and the answer is not obvious.

### Option A — Shims inside the package dir, with an `install.json` stage tri-state

| Pros | Cons |
|---|---|
| No new store | **Breaks a two-state invariant that has exactly one meaning today.** `check_install_status` (`pull.rs:324`, `pull.rs:361`) is a binary gate, and `pull.rs:338-342` treats *present-but-not-OK* as **crash recovery → re-pull**. A deliberate lazy stage is byte-indistinguishable from a crashed pull |
| Reuses `PackageDir` accessors | Breaks "package dir exists ⇒ content is present", which `store.content()`, `TemplateResolver::check_exists`, `find` and GC all rely on ([#302](https://github.com/ocx-sh/ocx/issues/302) makes the same argument) |
| | Forces a baked identifier into a content-keyed store; the package dir is the atomic-move unit (`move_temp_to_object_store`, `pull.rs:702-723`) and has no atomic producer for a half state |
| | GC's `PackageStore::list_all` + `is_valid_cas_path` (`cas_path.rs:93-114`) would walk half-packages as packages |

**Reject.** Buys "no new store" by corrupting the store's clearest invariant.

### Option B — Separate identity-keyed shim store + `ocx launcher shim` sibling verb **(CHOSEN — [#302](https://github.com/ocx-sh/ocx/issues/302))**

| Pros | Cons |
|---|---|
| Composes the **interface** view, so an entrypoint name lands on its real launcher and its dispatch `command` + baked `args` apply | One new wire verb; on Windows, one new sidecar (§D2) — both need day-one paired goldens |
| State detection is structural (S5), not a field someone forgets to check | Requires a committed Windows blob refresh before Windows laziness works (§D2, phased) |
| PATH shadowing makes steady-state cost zero (S7) | |
| `find_or_install` already does materialize-on-miss, interface compose and `--offline` refusal — the new runtime code is small (D-b) | |
| The trigger is the ordinary pull, so `pull.rs:95-116`'s three cross-process layers apply verbatim (D-c) | |

**Choose.**

### Option C — No shim store; laziness via `ocx pull` granularity or a project-local link tree

| Pros | Cons |
|---|---|
| Zero new surface | (i) Pre-pulling subsets pushes "which subset?" onto the user — a CI job that cannot know still over-pulls. Status quo with extra typing |
| (ii) reuses a tree already being designed | (ii) **a link cannot point at a target that does not exist**; the link tree presupposes materialization. Also project-local, so one digest lazy in N projects yields N trees, and no answer for `--global` |

**Reject** as the feature's negation. Kept as the reversibility floor.

### Option D — Shim entry as a stand-in package root consumed by the existing `launcher exec`

**Description.** Same store as B, but the entry is shaped as a package root that
`launcher exec` already accepts — so no new verb and no new sidecar: add
`$OCX_HOME/shims` to the existing enumerated pkg-root allow-list, and have
`launcher exec` materialize-then-redispatch when the root is a shim entry.

| Pros | Cons |
|---|---|
| Zero new wire vocabulary; both existing paired goldens keep covering it unchanged | **Fatal: `launcher exec` forces `self_view=true`** (`subsystem-cli-commands.md`, `launcher exec` row). Under the self view a root's own `entrypoints/` is dropped from `PATH` — `Entrypoints::IMPLICIT_VISIBILITY = INTERFACE` (`entrypoint.rs:186`), and `composer::carrier_crosses` puts a root's launchers on the interface surface only ("root's launchers absent under `--self`", `subsystem-package-manager.md` composer row) |
| The allow-list already admits three roots, so a 4th follows a documented pattern (`core.rs:141-160`, `exec.rs:244-250`) | Consequence: a shimmed **entrypoint** name would resolve to the raw binary in `bin/` instead of its launcher, **silently skipping the dispatch `command` and baked `args`**. A correctness bug that presents as "the tool ran, just wrong" |
| Still needs the same Windows blob refresh, so it saves nothing there | Redispatch-in-place also forfeits S7's PATH shadowing, reintroducing a permanent per-exec tax |

**Reject — corrected 2026-08-09.** The originally recorded reason (entrypoint dispatch
would be silently skipped) is **false** and is withdrawn: `launcher exec` reads the
dispatch `command` and baked `args` from `metadata.json` *before* any PATH lookup
(`command/launcher/exec.rs:140-153`) and then resolves the already-dispatched name
(`:230`), so dropping `entrypoints/` from PATH under `self_view=true` skips nothing.

The decision stands on the reason that actually holds: **a path cannot yield an
identifier.** `cas_shard_path` writes `{algorithm}/{hex[0..2]}/{hex[2..32]}` — 32 hex
characters regardless of algorithm, so 32 of 128 for SHA-512 — which is why
`write_digest_file` exists and why the layout contract states the full digest is not
recoverable from the path. The registry component is a lossy `to_relaxed_slug`
(`utility/string_ext.rs:23-33`, non-injective). This project already implemented
path-derived identity once and deliberately replaced it with in-directory `digest` +
`resolve.json`; Option D would reintroduce it. A shim must therefore carry the
identifier **as data**, which is only possible with a verb whose argument is an
identifier. For the same reason D does not "restore the E3 containment binding" — there
is nothing to bind against.

Two secondary reasons also survive: `launcher exec` composes `self_view=true`
unconditionally, and fusing would make composition mode depend on the root kind passed;
and `launcher exec` is today provably network-incapable, a property worth keeping behind
its own name and canary.
[#302](https://github.com/ocx-sh/ocx/issues/302)'s one-line justification — "a sibling
verb, not `launcher exec` — that one forces `self_view=true`" — is right, and this is
the concrete failure it prevents.

### Weighted trade-off summary

| Criterion (weight) | A | **B** | C | D |
|---|---|---|---|---|
| Entrypoint dispatch correctness (**high**) | ✓ | **✓** | n/a | ✗ silently wrong |
| Store-invariant integrity (**high**) | ✗ | **✓** | ✓ | ✓ |
| Semantic identity eager ⇄ lazy (**high**) | crash-recovery ambiguity | **✓ same pull** | n/a | ✓ |
| Concurrency correctness (**high**) | inherits | **inherits verbatim** | n/a | inherits |
| Delivers the feature (**high**) | ✓ | **✓** | ✗ | ✓ |
| Steady-state exec cost (**medium**) | zero | **zero (S7)** | n/a | permanent tax |
| New frozen contracts (**medium**) | 1 (`install.json` shape) | **1 (Windows sidecar)** | 0 | 0 |
| Windows blob refresh (**low**) | no | yes | n/a | yes |

---

## Decision Outcome

**Option B** — [#302](https://github.com/ocx-sh/ocx/issues/302) as filed, plus the
corrections below. Every correction is a Discover finding or a code fact; nothing
settled in [#302](https://github.com/ocx-sh/ocx/issues/302) is redesigned.

## Corrections

### D1 — Generation has no seam inside the pull pipeline *(corrects Discover finding 3)*

The brief flags that `resolve.json` lands late — `post_download_actions`
(`pull.rs:656-692`) runs at `pull.rs:525`, after assembly — so "prepare stops before
content" would cut `setup_owned_impl` in the wrong place. Correct, and the conclusion
goes further: **there is no cut to make.**

Generation needs exactly two things, both already standalone calls:

- `PackageManager::resolve(identifier, platform)` (`pull.rs:215`) → the pinned digest;
- `common::load_config_metadata(index, pinned, &manifest)` (`pull.rs:390`) → the
  metadata carrying `binaries` / `entrypoints` / `env`.

That is the same pair `ocx package inspect --closure` already composes for its
metadata-only walk (`subsystem-package-manager.md`, `tasks/inspect.rs` row). So
generation is a **sibling task**, not a cut: `setup_owned_impl` is not modified at
all, which is what makes D-a's identity guarantee structural rather than
aspirational. This is the concrete shape of
[#302](https://github.com/ocx-sh/ocx/issues/302)'s "[#28](https://github.com/ocx-sh/ocx/issues/28)
— metadata-first pull; the `prepare` phase is the same split", and it consumes the
same interface-closure projection [#177](https://github.com/ocx-sh/ocx/issues/177)
computes (one computation, two consumers).

### D2 — The Windows blocker: `.shim` cannot carry a pinned identifier *(new; Discover finding 6 lands here)*

[#302](https://github.com/ocx-sh/ocx/issues/302)'s layout says
`<name>.exe hardlink + <name>.shim on Windows`, and
[#301](https://github.com/ocx-sh/ocx/issues/301) lists "no change to the `.shim`
sidecar format or the `launcher exec` wire" as a non-goal. Under Option B those cannot
both hold, for three independent reasons:

1. The `.shim` format is frozen as **exactly one line: the absolute `pkg_root`**
   (`adr_windows_exe_shim.md` §`.shim` Sidecar Format Contract; `body.rs:65-82`). A
   pinned identifier is not a path.
2. `parse_sidecar` rejects a non-absolute value → **E2 `MalformedSidecar`, exit 78**
   (`main.rs:51-53`).
3. Even if it parsed, `pkg_root_allowed` (`core.rs:155-160`) admits exactly three
   roots — `packages`, `temp/test`, `temp/patch-test` — so a shim-store value trips
   **E3, exit 77**. And the exe emits the hard-coded token
   `WIRE_SUBCOMMAND = "launcher exec"` (`core.rs:189`), never `launcher shim`.

All three live in the committed blobs, so **Windows laziness requires a shim source
change and a blob refresh regardless of design.** Given that, do not narrow `.shim`:

> **Decision: a second sidecar, `<name>.shimref`**, holding one line — the pinned
> identifier. `.shim` keeps meaning exactly one thing and every installed sidecar
> stays valid. The exe reads whichever is present: `.shim` → `launcher exec` (today,
> byte-identical); `.shimref` → `launcher shim`. One `if exists`, two wire tokens,
> explicit state instead of an overloaded field type.

Reuse `.shim`'s byte contract verbatim (UTF-8, no BOM, single line, trailing LF,
32 KiB read cap, no interior `\n`/`\r`/`\0`) and change only the payload's meaning.

**The tamper surface changes shape, and must be closed.** E3 confined a `.shim`
target to `$OCX_HOME/packages`. A `.shimref` names a *package*, so a tampered one
would make ocx fetch and run an attacker-named package — no path check applies.

**Amended 2026-08-09 — the proposed closure does not exist.** This section originally
decided that `launcher shim` would refuse an identifier not matching its own shim-store
directory, calling it free closure. Cross-model and spec review showed it is
*unimplementable as specified*: the wire (C-010) passes a pinned identifier and a
basename, so the invoking shim's own path never crosses the interface — there is no
"own directory" to compare against. Passing `dirname "$0"` would close that gap
mechanically while authenticating nothing, since whoever can write the shim controls
both operands. The residual was large anyway: `to_relaxed_slug` is not injective
(`utility/string_ext.rs:23-33`) and the CAS path carries 32 of 64 hex digits.

**Decision, revised: the tamper surface is not closed by a guard.** Write access to
`$OCX_HOME/shims/**` is a trust boundary equal to write access to
`$OCX_HOME/packages/**`. Integrity rests where it already does — the full-digest fetch
and its content verification. The verifiable guard that replaces it is `argv0`
validation (parse as `BinaryName`, require membership in the composed name set), which
is cheap, real, and testable red-and-green.

**Canaries, day one.** The `launcher shim` token joins the wire-ABI canary rule
(`subsystem-package-manager.md`) with paired goldens on both producers, exactly as
`launcher exec` has (`body.rs::tests::launcher_wire_token_is_bound_to_shim_producer`
⇄ `ocx_shim::tests::shim_wire_token_matches_sh_body`). Separately, and worth fixing
while both files are open: the pkg-root allow-list is today held in sync across two
crates **by a doc comment alone** — `core.rs:141-160` claims it "mirrors
`validate_launcher_pkg_root`'s allow-list in `ocx_cli` exactly", with no test
enforcing it. That is a pre-existing gap in a security boundary; close it with a
paired golden plus a red-and-green containment test (`quality-core.md` "Unchecked
Green").

### D3 — [#301](https://github.com/ocx-sh/ocx/issues/301)'s copy fallback: drop it *(Discover finding 1)*

[#301](https://github.com/ocx-sh/ocx/issues/301) proposes a copy fallback "when
hardlinking is unavailable (FAT/exFAT home, some network mounts)".
`hardlink.rs:11-15` states the standing doctrine: `$OCX_HOME` is single-volume — a
constraint the `temp → packages/` atomic rename already imposes — and a cross-device
split must surface a clear error rather than silently degrade.

The rebuttal is specific, not doctrinal: **on the homes the fallback names, nothing
else works either.** Package assembly hardlinks every content file
(`assemble_from_layers_with_layouts`, `pull.rs:470`), and the atomic rename is the
publication step for every package. A copy fallback would buy a working shim binary
inside an `$OCX_HOME` where no package can be installed at all.

**Decision: no copy fallback.** `CrossesDevices` propagates as everywhere else, exit
74. Regeneration goes through `utility::fs::persist_temp_file` rather than in-place
overwrite: all N names now share one inode, so a *running* `<name>.exe` locks the
inode behind every sibling name, and that `ERROR_SHARING_VIOLATION` /
`ERROR_ACCESS_DENIED` retry already lives there.

Bonus worth recording: one inode means one Authenticode signature and one Defender
scan instead of N — hardlinking strictly helps the signing story
(`adr_windows_exe_shim.md` Axis A1).

### D4 — `.bin` GC: collect nothing *(Discover finding 2, reframed)*

[#301](https://github.com/ocx-sh/ocx/issues/301) proposes collecting a `.bin` entry
when its link count is 1. The brief cites the research rejection of `nlink`; that
rejection needs narrowing to be fair. Research §4 rejects `nlink` because it *cannot
attribute a shared blob to a specific logical owner*. Here there is no owner question
— "does any launcher still link these exact bytes" is precisely what `nlink` answers.
[#301](https://github.com/ocx-sh/ocx/issues/301)'s reasoning is **not** the rejected
pattern, and the finding overstates its case.

It is nevertheless **unnecessary**, and one rung lazier is available: the set is
bounded by (ocx versions ever run × arch), a handful of ~300 KB files, each
regenerable from `include_bytes!`. Collecting nothing is zero code and zero race — an
`nlink==1` observation landing between temp-write and hardlink would collect a live
entry. This is exactly the case `research_lazy_digest_fetch_and_gc.md` §4 recommends
collecting nothing for.

**Decision: no GC for `$OCX_HOME/.bin/ocx-shim/`.** The shim *store*'s own liveness
rule (S10) is unaffected and unchanged.

### D5 — Auth keeps the existing degrade *(Discover finding 7)*

`auth.rs:51-64` (`get_or_fallback`) swallows chain errors into an anonymous attempt
with a warning; `auth.rs:100-111` maps a credential-helper `HelperFailure` to `None`
at **debug** level, so a broken helper is quieter still. Either way a misconfigured
helper surfaces as a registry 401 rather than an auth error — a poor signal from
inside a build.

**Decision: the lazy path keeps it, unchanged.** Not because the degrade is good, but
because D-a forbids the fix here: changing auth semantics only on the lazy path would
make a lazy materialization behave differently from the eager install it defers. The
confusing-401 is not a laziness bug — it bites eager installs identically — and is
recorded as an out-of-scope follow-up against `auth.rs:100-111` (the debug-level
`HelperFailure` arm is the sharper half), to be fixed once for both paths.

No blocking prompt is reachable from a spawned child today, and that stays true —
correct by construction (`research_lazy_digest_fetch_and_gc.md` §3).

### D6 — `ocx package which`: keep `kind`, decide the wire-shape break *(Discover finding 4, half-withdrawn)*

The brief reads finding 4 as grounds to drop `kind`. That over-reads it: S12 is
settled and `kind` stays. But the finding contains one real code fact that
[#302](https://github.com/ocx-sh/ocx/issues/302) does not appear to weigh:

`Paths` serializes as a **map** `{package: path}` (`api/data/paths.rs:38-47`). Adding
a per-entry field is a wire-**shape** change — map → array of objects — and
`which.rs:28` documents the map form *in the command's own help text*:

```
cmake_root=$(ocx package which --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"]')
```

Every script following that documented recipe breaks. Per CLAUDE.md, "if a published
artifact or someone's script can observe it, weigh it — then still just make it, and
write the changelog line". So: **make the break, announce it in the commit subject,
and update the help-text example in the same change.**

> **AMENDED 2026-08-09 — the shape chosen is the map, not the array.** The sentence
> above stands on the *break*; the paragraph before it was wrong about the *shape*. It
> asserted that adding a per-entry field forces map → array. It does not: `PathEntry`
> already carries `package` and `path` as struct fields (`api/data/paths.rs:17-20`) and
> only `Paths`' hand-written `Serialize` flattens the entry to a bare path
> (`paths.rs:38-47`). Making the map's *value* an object is therefore available, and it
> is what ships. Three reasons: (1) **convention** — every other multi-package report in
> this CLI is a keyed object (`package inspect`, `package info`), so an array would make
> `which` the only divergent shape, which is the divergence CLAUDE.md's output rules
> exist to prevent; (2) **consumer cost** — `jq -r '.["cmake:3.28"]'` becomes
> `jq -r '.["cmake:3.28"].path'`, one token, where an array forces every consumer to
> rewrite lookup as a `select()` scan; (3) the original reason was **a factual error**,
> surfaced by the architecture review, and a decision may not rest on one. The break is
> still a break and is still announced in the commit subject. Deviation 5 below and
> `PLAN-NC-1` in the plan carry the same amendment.

On the naming half of the finding: `EnvEntry` has a Rust field `kind` serialized as
`"type"`. `PathEntry.kind` serializing as `"kind"` is not a conflict — different
documents, genuinely different concepts that happen to share an English word. Not
worth a rename; recorded so a reviewer does not re-raise it.

### D7 — Verified: the `--frozen` / `--offline` posture *(Discover finding 8)*

S9's table is **correct**, verified against `crates/ocx_lib/src/oci/index.rs`:

| Mode | Digest-addressed miss | Citation |
|---|---|---|
| `Frozen` | walks the source — "digest-addressed content is still fetched from the source exactly like `Default`… frozen still pulls locked digests" | `index.rs:83-90` |
| `Offline` | "Local index only… misses return `None` for digest-addressed content" | `index.rs:77-82` |

So a shim **can** materialize by digest alone under `--frozen` and **must** refuse
under `--offline`. `--frozen` + lazy is therefore the motivating CI shape: `ocx lock`
resolves every tag eagerly and writes digests; compose writes shims from those
digests; the build materializes only what it invokes. No lazy-specific network flag is
invented (`research_lazy_digest_fetch_and_gc.md` recommendation 2). `--offline`
propagates to a shim correctly because `OCX_OFFLINE` reaches the child through
`Env::apply_ocx_config` — which is what makes S9's "a shim's resolution posture equals
a plain `ocx` invocation" true rather than aspirational.

### D8 — Shim filenames follow the `BinaryName` grammar *(Discover finding 9)*

`EntrypointName` is `^[a-z0-9][a-z0-9_-]*$`; `BinaryName` is deliberately looser to
admit `c++`, `python3.13`, `MSBuild` (`adr_declared_binaries_metadata.md` §5). Since
S6 routes both sets through one path, **shim filenames follow the looser grammar**.
`BinaryName` already forbids `/`, `\` and the Windows-reserved set at construction,
closing the npm/pnpm bin-field path-traversal CVE family by grammar
(`adr_declared_binaries_metadata.md` §5 security note) — so no second validator and no
new sanitization for the shim store.

### D9 — `ocx_shim`'s exit taxonomy stays separate *(Discover finding 10)*

`ShimError` maps to 78/77/74/69 (`main.rs:48-107`), independent of
`ocx_lib::ExitCode`. The `.shimref` path (D2) adds **no new shim failure mode**: a
malformed `.shimref` is E2 (78); a mismatched identifier is refused by `launcher shim`
in `ocx_lib`'s taxonomy, not the shim's. Recorded so the implementation does not invent
a parallel code.

## Withdrawn

Three positions from the pre-issue draft and the brief's finding list, withdrawn
against the filed [#302](https://github.com/ocx-sh/ocx/issues/302). Recorded rather
than silently dropped.

| Position | Why withdrawn |
|---|---|
| **Discover finding 5 — `--lazy-mode <MODE>` deviates from the `--X`/`--no-X` convention** | The convention (`subsystem-cli.md` "Paired Boolean Toggles") governs **boolean and tri-state toggles**. `lazy-mode` is an open-ended strategy enum resolved across four `Option<LazyMode>` tiers (S4), which a flag pair cannot express. Not a deviation — a different kind of option. My `--lazy`/`--no-lazy` counter-proposal, and the `lazy-mode` → `lazy` rename that followed from it, are both withdrawn. |
| **A structural eligibility test** (auto-eager when a package declares an `${installPath}`-rooted non-`Path` var) | [#302](https://github.com/ocx-sh/ocx/issues/302) rules it out with an argument I did not have: a publisher adding `FOO_HOME=${installPath}/share` in v1.2 would silently flip a tool from lazy to eager with no diagnostic. The advisory (S3) surfaces the same information without making laziness an emergent property of someone else's metadata, and the publisher-side lever (mark the var `private`) already exists. Withdrawn in favour of S3. |
| **An unconditional stderr line on first exec** | Right that silence reads as a hang (`research_lazy_shim_prior_art.md` trap 4); wrong about the channel. S11 is correct: a shim runs inside somebody's `$(...)` or CI log parser and that fd 2 is theirs. Silent default plus `progress` on the controlling terminal is the right answer. Withdrawn. |

### Quantified Impact

| Metric | Before (eager) | After (`lazy-mode = "always"`) |
|---|---|---|
| Bytes fetched, N-tool toolchain, K invoked | all N packages' layers | N config blobs (≤4 MiB cap each, `common.rs` `MAX_METADATA_BLOB_BYTES`) + K packages' layers |
| Registry requests | N manifest + N×L layer GETs | N manifest GETs + K×L layer GETs — deferral does not discount a digest GET |
| Windows shim bytes, E entrypoints | E × 235–329 KB | one blob + E hardlinks ([#301](https://github.com/ocx-sh/ocx/issues/301)); one Defender scan instead of E |
| Per-exec overhead, warm | 0 | **0** — PATH shadowing (S7), not a permanent indirection |
| Warm compose overhead | 0 | one `stat` per tool entry (S5's atomic-rename-means-complete) |
| Composed env determinism | function of lock | function of `(lock, lazy-mode)` — cache state changes nothing (S8) |
| New GC tiers / `refs/` kinds | — | 0 new tiers; one existing predicate + one existing forward-ref pattern (S10) |
| `--offline` + unmaterialized tool | n/a | exit **81** at exec time |

### Consequences

**Positive**
- `--frozen` + lazy: fully lock-pinned, fetching only what runs.
- Zero steady-state cost — the one thing every surveyed competitor gets wrong.
- [#301](https://github.com/ocx-sh/ocx/issues/301) rides along and improves the
  signing story.
- The pkg-root allow-list gets the paired canary it should already have had (D2).

**Negative**
- `command -v <tool>` succeeds before the tool exists. Structural to every shim design
  (`research_lazy_shim_robustness.md` constraint 3); state it as the contract.
- An unverified `binaries` claim becomes an observable `PATH` entry that fails *after*
  a download. [#302](https://github.com/ocx-sh/ocx/issues/302)'s answer is right: a
  clean error naming the package and the unfulfilled claim, and
  `ocx package create --bin-scan` (Verify) as the recommended publisher posture.
- Windows laziness is gated on a shim source change + blob refresh (D2), phased.
- `ocx package which` JSON breaks shape (D6).
- The dereference class (`$JAVA_HOME/bin/java`, `.pc` files, `DYLD_*`) is never
  covered — the publisher-side lever is marking the var `private`.

**Risks**
- *A build blocks on a first exec inside `make -j`.* Correct by construction (D-c),
  but [#302](https://github.com/ocx-sh/ocx/issues/302) is right that singleflight
  **poisoning** ([#52](https://github.com/ocx-sh/ocx/issues/52)) becomes load-bearing
  once first-exec is a concurrent entry point: today a failed leader broadcasts the
  failure to every waiter (`pull.rs:259-262`), so one transient failure under
  `make -j16` fails all sixteen rather than letting fifteen retry. I read
  [#52](https://github.com/ocx-sh/ocx/issues/52) as a **prerequisite**, not a related
  issue — see `[NEEDS CLARIFICATION 2]`.
- *Allow-list drift between `ocx_shim` and `ocx_cli`.* Real today, unmitigated.
  Mitigation: D2's paired golden, red-and-green.
- *`.shimref` v1 turns out wrong.* It is a One-Way-Door artifact like `.shim`.
  Mitigation: reuse `.shim`'s over-specified byte contract verbatim, changing only the
  payload's meaning, so the grammar is already battle-tested.

### How Would We Reverse This?

`lazy-mode` already defaults to `never`, so reversal is: stop emitting the `shims/`
PATH slot, delete `$OCX_HOME/shims/`, delete the `launcher shim` verb and the
`.shimref` branch. Nothing published, no lock schema, no metadata field, no change to
`launcher exec` or `.shim`. Option C is the reversal target and it is the pre-change
status quo. [#301](https://github.com/ocx-sh/ocx/issues/301) reverses independently
(stop hardlinking; existing hardlinks keep working).

---

## Constitution Check — `arch-principles.md`

| Principle | Compliance |
|---|---|
| Crate layout / dependency direction | ✓ `ocx_lib` + CLI options/command + `ocx_shim` (D2) |
| Composite root (`FileStructure`) | ✓ new stores as fields built in `with_root`; **never** `ShimStore::new(root.join("shims"))` at a call site (Block-tier, `subsystem-file-structure.md`) |
| Facade (`PackageManager`) | ✓ generation is a `tasks/` module; only `pub` methods on the shared `impl`, helpers as free functions |
| Three-layer errors | ✓ new `PackageErrorKind` variants; per-package diagnosis preserved in `_all` |
| Command pattern | ✓ args → identifiers → manager task → report data → `Api` |
| Ref separation for GC | ✓ `shims/…/refs/blobs/` is the existing forward-ref pattern; no new ref kind, no new CAS tier |
| Module structure (one concept per file, no `mod.rs`) | ✓ `file_structure/shim_store.rs`, `tasks/prepare_lazy.rs`, `options/lazy_mode.rs` |
| Internal enum exhaustiveness | ✓ `LazyMode`, `LazyReport` closed (no `#[non_exhaustive]`); error enums keep it |
| Utility catalog before inventing | ✓ `hardlink::update`, `persist_temp_file`, `move_dir`, `SerdeExt`, `cas_shard_path`, `repository_path`, `slugify` |
| Locking policy | ✓ atomic-rename-replaced data → `lock_scoped` into `$OCX_HOME/locks`, never a sidecar |
| Test-only seams | ✓ any forced state uses `__OCX_*` + `cfg(any(test, feature = "__testing"))` |
| No `deny_unknown_fields` reachable from `Config` | ✓ `lazy-mode` / `lazy-report` are additive |
| Fleet forward-compat | ✓ an older binary reading a newer project file ignores `lazy-mode` and composes eagerly — degrades to correct, never to broken |

### Constitution Deviations

| # | Deviation | Justification | Mitigation |
|---|---|---|---|
| 1 | A **second launcher sidecar** (`.shimref`) alongside the frozen `.shim` | Option B needs an identifier where `.shim` is contractually a path (D2). The alternative — redefining `.shim`'s payload — narrows an artifact already on disk in every Windows install, the One-Way-Door direction `adr_windows_exe_shim.md` explicitly declines to take | Reuse `.shim`'s byte contract verbatim, changing only the payload's meaning; day-one goldens; `.shim` semantics untouched so every installed sidecar stays valid |
| 2 | A **second wire verb** (`launcher shim`) beside `launcher exec` | `launcher exec` forces `self_view=true`, which drops the root's `entrypoints/` from `PATH` and would silently skip entrypoint dispatch (Option D) | Joins the wire-ABI canary rule with paired goldens on both producers, exactly as `launcher exec` has |
| 3 | A store keyed by registry **+ repository** + digest, where `PackageStore` deliberately omits the repository | A digest-pinned re-pull needs the repository — an OCI digest GET is `/v2/<name>/manifests/<digest>`. Mirrors `SymlinkStore`'s keying, not `PackageStore`'s: different question, different key ([#302](https://github.com/ocx-sh/ocx/issues/302) S5 frames this as content-vs-identity) | `subsystem-file-structure.md` gains the store row with this rationale so a reviewer reads it as intentional |
| 4 | A **new tamper surface**: a sidecar naming a package rather than a contained path | Forced by D2 — an identifier has no containment root to check | **Not mitigated by a guard** (amended 2026-08-09; the originally proposed directory binding is unimplementable — see D2). Accepted as a trust boundary equal to write access to `$OCX_HOME/packages/**`; integrity rests on the full-digest fetch and its content verification. The verifiable guard retained is `argv0` validation (`BinaryName` grammar + membership in the composed name set) |
| 5 | `ocx package which` JSON changes **shape** (map → array), breaking a recipe printed in the command's own help | S12 is settled and a per-entry field cannot be added to a map. Pre-1.0, and CLAUDE.md's test is "weigh it — then still just make it, and write the changelog line" | Changelog line via the commit subject; `which.rs:28`'s `jq` example updated in the same change; `[NEEDS CLARIFICATION 1]` offers the one alternative |

No deviation is left unjustified. Deviations 1, 2 and 4 carry the security and
contract weight, and each gains a test that can go red.

---

## Migration / Rollout

**Nothing breaks at the default.** `lazy-mode` resolves to `never` absent every tier
(S4), so an unconfigured project behaves exactly as today.

| Surface | Impact |
|---|---|
| OCI manifests / package metadata | **None.** `binaries` and `entrypoints` already ship — `binaries` becomes load-bearing, but its wire form is unchanged |
| `ocx.lock` | **None.** Generation consumes the digests the lock already pins |
| Existing installs / existing launchers | **None.** `launcher exec` wire and `.shim` format untouched; existing per-entrypoint blob copies keep working ([#301](https://github.com/ocx-sh/ocx/issues/301): no migration) |
| New local artifacts | `$OCX_HOME/shims/**`, `$OCX_HOME/.bin/ocx-shim/**` — local-only, derived, regenerable |
| Config grammar | Additive: `lazy-mode` / `lazy-report` at toolchain, `[group.<g>]`, `[package."<id>"]`; ignored by older binaries |
| CLI grammar | Additive: `--lazy-mode`, `--lazy-report` |
| **`ocx package which` JSON** | **Breaking shape change** — the top-level map is *kept*, keyed by the requested identifier; the value changes from a path string to `{path, kind, package}`. Deviation 5, amended 2026-08-09 (was: map → array of objects) |

**Phasing** (mirrors how the `.exe` shim itself shipped — `adr_windows_exe_shim.md` D1):

1. **Prerequisite** — [#52](https://github.com/ocx-sh/ocx/issues/52) singleflight
   poisoning. Not merely "related": first exec makes concurrent entry the normal case.
2. **[#301](https://github.com/ocx-sh/ocx/issues/301)** — `.bin` store + hardlinked
   launchers, no copy fallback (D3), no GC (D4). Lands standalone and is independently
   valuable.
3. **Unix laziness** — shim store, generation task, `launcher shim`, `lazy-mode` /
   `lazy-report` tiers, PATH slot, advisory, `which` policy + `kind`, GC liveness. A
   Windows host with `lazy-mode = "always"` composes eagerly and logs at debug — no
   user-visible breakage.
4. **Windows laziness** — `.shimref` in `ocx_shim`, the `launcher shim` token, blob
   rebuild + `SHIM_SHA256` refresh (D2), Windows shim emission, plus the allow-list
   paired golden.

**Announcement.** Breaks and features are announced in the changelog and nowhere
else, and the changelog entry **is the commit subject** — `CHANGELOG.md` is generated
by `git-cliff` and must never be hand-edited (CLAUDE.md, "⛔ Never edit
`CHANGELOG.md`"). No migration prose in user docs
(`feedback_no_migration_prose_in_docs`): the contract is documented as present
behaviour, not as a change.

Suggested subjects:
`feat(cli): tools can defer download until a declared binary is first run`
`feat(cli)!: package which reports one object per package, with its kind`
`perf(windows): entrypoint shims hardlink one shared binary instead of copying it`

### Documentation Surfaces

[#302](https://github.com/ocx-sh/ocx/issues/302)'s own list is complete; reproduced so
the plan can check it off, with three additions marked.

- `reference/env-composition.md` — PATH order, the shim slot, `--self`.
- `reference/configuration.md` — `lazy-mode` / `lazy-report` tiers and precedence.
- `reference/environment.md` — `OCX_LAZY_MODE`, `OCX_LAZY_REPORT` (**mandatory**:
  resolution-affecting, `quality-rust.md` checklist last row → also `OcxConfigView`
  + `Env::apply_ocx_config`).
- `reference/command-line.md` — `--lazy-mode`, `--lazy-report`, `which`'s `kind`
  **and its changed JSON shape** *(added)*.
- `reference/metadata.md` — `binaries` becomes load-bearing.
- `user-guide.md` — when to use it, and the dereference boundary.
- Storage-layout page — `$OCX_HOME/shims/`, `$OCX_HOME/.bin/` *(added)*.
- `.claude/rules/subsystem-file-structure.md` — new store rows + Deviation 3 rationale.
- `.claude/rules/subsystem-package-manager.md` — generation task row; **extend the
  Wire-ABI canary section**: `launcher shim` producers, and the allow-list pairing.
- `.claude/rules/subsystem-cli.md` / `subsystem-cli-commands.md` — `--lazy-mode` per
  command; `which` shape.
- `.claude/rules/arch-principles.md` — ADR index row; store/utility tables.
- `crates/ocx_cli/src/command/which.rs:28` — the `jq` example in the help text *(added)*.

---

## Reconciliation with `adr_project_toolchain_links.md` (Proposed)

That ADR's **D3** says entrypoints are "not a shim distribution vehicle", and its
**Option E** rejects shims-for-everything. Both stand; neither is contradicted.

- **D3 is honoured.** This ADR does not turn entrypoints into a distribution vehicle.
  A lazy shim is a third artifact with a third meaning: entrypoints mean "this binary
  needs the composed env"; a lazy shim means "this package is not on disk yet". S7 is
  the proof they stay distinct — the shim is *shadowed by* `entrypoints/` the moment
  content appears, so the two never hold the same role at the same time.
- **Option E was rejected for materialized packages, and every reason holds.** It was
  rejected because (i) shims cannot serve the dereference class so links are needed
  anyway, and (ii) a per-exec ocx spawn on every binary is the asdf 120–150 ms lesson.
  Both are about shimming packages *already on disk*, where a real directory exists to
  put on `PATH`. Neither applies to a package with no directory at all — and S7's
  shadowing means the steady-state per-exec cost that killed Option E is **zero
  here**, which is precisely the property Option E lacked.
- The two ADRs reach the same conclusion about the dereference class from opposite
  directions: **nothing but a real directory can serve `JAVA_HOME`.** That ADR answers
  with a stable link; this one answers with an advisory (S3) plus the publisher-side
  `private` lever.
- **They compose.** The toolchain link tree points at a materialized package root; a
  lazy tool has no link until it materializes. That ADR's heal-before-emit pass (its
  D6) is the natural place to reconcile a link whose target is still deferred, and
  neither mechanism needs the other's internals.

---

## Open Questions

`[NEEDS CLARIFICATION 1]` — **CLOSED 2026-08-09.** `ocx package which`'s JSON keeps its
top-level map and gives the value an object shape
(`{"cmake:3.28": {"path": …, "kind": …, "package": …}}`). The premise this question was
written on — "S12's `kind` field *cannot* be added to the current map form" — is false:
`PathEntry` already holds both fields as struct members (`paths.rs:17-20`); only the
hand-written `Serialize` flattens them away. So this was never possible-versus-impossible,
only a cost choice, and the map wins it on convention, on consumer cost, and on not
resting a decision on a wrong premise. Full reasoning at D6's amendment above and at
`PLAN-NC-1` in `.claude/state/plans/plan_lazy_package_loading.md`.

`[NEEDS CLARIFICATION 2]` — **Is [#52](https://github.com/ocx-sh/ocx/issues/52) a
prerequisite or a parallel track?** [#302](https://github.com/ocx-sh/ocx/issues/302)
lists singleflight poisoning as *related*; I read it as **blocking**, because
`pull.rs:259-262` broadcasts a leader's failure to every waiter, so one transient
failure under `make -j16` fails all sixteen builds rather than letting fifteen retry.
That exposure is unique to first exec. Confirm the sequencing — it changes the plan's
critical path, not its design.

---

## Implementation Plan (high level — the detailed plan is a separate artifact)

1. [ ] [#52](https://github.com/ocx-sh/ocx/issues/52) singleflight poisoning (prerequisite — `[NEEDS CLARIFICATION 2]`).
2. [ ] [#301](https://github.com/ocx-sh/ocx/issues/301): `$OCX_HOME/.bin/ocx-shim/<sha256>.exe` + `hardlink::update` via `persist_temp_file`; **no copy fallback** (D3); **no GC** (D4).
3. [ ] `file_structure/shim_store.rs` + the `FileStructure` field; path goldens; atomic-rename generation (S5).
4. [ ] `tasks/prepare_lazy.rs` — `resolve` + `load_config_metadata` + entry write + `refs/blobs/` (D1), sharing [#177](https://github.com/ocx-sh/ocx/issues/177)'s interface-closure projection.
5. [ ] `options::LazyMode` / `LazyReport` + the four-tier precedence chain (S4) + `OcxConfigView` + `apply_ocx_config`.
6. [ ] Advisory detector with typed reasons, one implementation, surfaced in `--format json` (S3).
7. [ ] `ocx launcher shim` + Unix launcher body + wire-token paired goldens; identifier-vs-directory refusal (D2).
8. [ ] PATH slot with INTERFACE visibility and the S7 push order; `--self` exclusion.
9. [ ] `ocx package which` policy + `kind` + shape change + help-text example (D6).
10. [ ] `ocx clean` shim liveness on the existing `collect_project_roots` predicate (S10).
11. [ ] **Windows**: `.shimref` in `ocx_shim`, `launcher shim` token, blob rebuild + `SHIM_SHA256`; allow-list paired golden + red-and-green containment test (D2).
12. [ ] Documentation surfaces (list above).

## Validation

[#302](https://github.com/ocx-sh/ocx/issues/302)'s acceptance criteria are adopted
verbatim. Added here, from the corrections:

- [ ] A shimmed **entrypoint** name applies its dispatch `command` and baked `args` —
      the assertion Option D would have failed. Assert on the *effect* of the baked
      args, not on the resolved path.
- [ ] Lazy and eager materialization of the same identifier produce **byte-identical**
      `packages/<digest>/` trees (D-a's identity claim, proven not asserted).
- [ ] After materialization the shim is not executed again — by **process trace**, not
      timing ([#302](https://github.com/ocx-sh/ocx/issues/302)'s own wording; the S7
      shadowing claim is otherwise unfalsifiable).
- [ ] Concurrent first exec: N processes, one download — asserted on the **transport
      call count**, not wall time.
- [ ] A `.shimref` whose identifier does not match its own directory is refused —
      shown **red and green** (Deviation 4).
- [ ] The allow-list paired golden fails when either producer changes alone.
- [ ] Cross-device `$OCX_HOME` surfaces `CrossesDevices`, never a silent copy (D3).
- [ ] Windows: E entrypoints yield one canonical blob + E hardlinks sharing one file
      index; a running `<name>.exe` does not block regenerating a sibling.
- [ ] `ocx clean` collects a shim dir only when no live lock pins it, and collecting
      one never breaks a subsequent compose.
- [ ] `task verify` green; existing `body.rs` / `generate.rs` / `ocx_shim` goldens
      **unmodified** — every change here is additive.

## Links

- Research: [`research_lazy_shim_prior_art.md`](./research_lazy_shim_prior_art.md), [`research_lazy_shim_robustness.md`](./research_lazy_shim_robustness.md), [`research_lazy_digest_fetch_and_gc.md`](./research_lazy_digest_fetch_and_gc.md)
- Constraining ADRs: [`adr_declared_binaries_metadata.md`](./adr_declared_binaries_metadata.md), [`adr_package_entry_points.md`](./adr_package_entry_points.md), [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md), [`adr_three_tier_cas_storage.md`](./adr_three_tier_cas_storage.md), [`adr_project_gc_symlink_ledger.md`](./adr_project_gc_symlink_ledger.md), [`adr_inspect_metadata_closure.md`](./adr_inspect_metadata_closure.md)
- Sibling (Proposed): [`adr_project_toolchain_links.md`](./adr_project_toolchain_links.md) — reconciled above
- Issues: [ocx-sh/ocx#301](https://github.com/ocx-sh/ocx/issues/301), [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302), [ocx-sh/ocx#52](https://github.com/ocx-sh/ocx/issues/52), [ocx-sh/ocx#177](https://github.com/ocx-sh/ocx/issues/177), [ocx-sh/ocx#28](https://github.com/ocx-sh/ocx/issues/28), [ocx-sh/ocx#69](https://github.com/ocx-sh/ocx/issues/69)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-09 | Architect (Opus 5) | Draft written without access to the issue bodies. Proposed an alternative (Option D) reusing `launcher exec`, a structural eligibility test, a `--lazy`/`--no-lazy` pair, and relocating `which`'s `kind` to `ocx status`. |
| 2026-08-09 | Architect (Opus 5) | Rewritten against the filed [#301](https://github.com/ocx-sh/ocx/issues/301) / [#302](https://github.com/ocx-sh/ocx/issues/302). **Option D withdrawn** — `launcher exec` forces `self_view=true`, which would silently skip entrypoint dispatch; kept as a considered-and-rejected option so the extra verb does not read as gold-plating. Eligibility test, flag pair and first-exec stderr line withdrawn in favour of S3/S4/S11. Retained and sharpened: the generation seam (D1), the copy-fallback rebuttal (D3), `.bin` collect-nothing (D4), the auth degrade (D5), the posture verification (D7). **New**: the Windows `.shim`-cannot-carry-an-identifier blocker with the `.shimref` proposal and its identifier-vs-directory tamper closure (D2); [#52](https://github.com/ocx-sh/ocx/issues/52) raised from related to prerequisite. |
