# Restructuring

You loaded this because you are about to move Rust code at scale —
extracting a type, decomposing a god struct, splitting a crate, or running
a mechanical change across many files.

Structural change is the one kind of work where "it compiles and the tests
pass" is least trustworthy: the type system checks that the pieces still
fit, not that they still do the same thing. Everything below assumes a
**parity oracle** — characterization snapshots of stdout, stderr, exit
code and filesystem effects, captured from the *pre*-change binary and
committed before the first code-move commit. Build it, then prove it can
fail by injecting a fault. If you cannot state the fraction of injected
faults it catches, you do not have an oracle; you have a build check.

Contents: [Diagnose First](#diagnose-first) · [Rules of the Move](#rules-of-the-move) ·
[Tooling Order](#tooling-order) ·
[Free-Function Cluster → Type](#free-function-cluster--type) ·
[God Struct → Cooperating Types](#god-struct--cooperating-types) ·
[Parallel Families → Trait](#parallel-families--trait) ·
[Module → Crate](#module--crate) · [Crate → Workspace](#crate--workspace) ·
[Hazards That Compile](#hazards-that-compile)

## Diagnose First

Measure the codebase; do not accept a description of it, including your
own. The two structural defects look opposite and need opposite fixes:

```bash
# Free-function clusters: which modules thread the same params?
rg -o --no-filename '^\s*(pub(\(crate\))?\s+)?(async\s+)?fn\s+\w+\(([^,)]+,\s*[^,)]+)' --type rust --glob '!external/**' . \
  | sed -E 's/.*fn \w+\(//' | sort | uniq -c | sort -rn | head

# Impl sprawl: which type absorbed everything?
rg -o --no-filename '^impl(<[^>]*>)?\s+[A-Za-z_]\w*' --type rust --glob '!external/**' . \
  | rg -v ' for ' | sort | uniq -c | sort -rn | head
```

A type with dozens of `impl` blocks across dozens of files needs
*decomposition*. A module of free functions threading the same pair needs
a *type*. A pipeline of independent pure steps needs neither — leave it
alone, and say so, or a later pass will "fix" it.

Write the diagnosis down with the numbers before planning anything, and
list by name the clusters that look wrong and are not. Without that list,
a worker will restructure a deliberate functional core.

## Rules of the Move

1. **One hat at a time.** A move commit changes no trust boundary, no I/O
   boundary, no wire format, no exit-code mapping. Not "mostly" — filter
   the diff to those files and confirm it is empty. Bundling a boundary
   change with a move makes every regression ambiguous between the two.
2. **Move, do not rewrite.** Bodies travel verbatim. A behaviour change
   riding along inside a move is invisible to every gate.
3. **Export, never copy.** A private-item error is fixed by widening the
   original's visibility or moving it — never by inlining a second copy. A
   copy compiles and silently forks behaviour the first time one is edited.
4. **Shim the old path.** `#[deprecated] pub use` at every old location,
   removed in one final, separately revertible commit.
5. **No stubbing.** "Make it compile" is not licence to empty a function
   body. Any `todo!()`, `unimplemented!()` or `Ok(Default::default())` that
   appears during a move is a defect, not progress (LINT-05).
6. **Run the oracle at every merge point**, not only at the end.
   Localising a regression one move later is far cheaper than after fifty.
   Merge in dependency order, leaves first — not completion order.

Validate the conventions on **two to five representative units** with full
review before fanning out. A systemic error costs orders of magnitude more
to fix after a thousand files than after three.

## Tooling Order

1. **rust-analyzer SSR** — `$pat ==>> $repl`, name-resolution-aware, so it
   distinguishes two identically-spelled paths. First choice for renames,
   `use`-path updates, and signature-preserving call-site rewrites.
2. **`ast-grep`** — syntax-aware patterns with `--update-all`, good for
   shapes SSR cannot express.
3. **`cargo fix --edition`** for edition migration — with the caveat that
   doctests, `build.rs`, and macros are explicitly outside its scope and
   need a manual pass afterwards. It reports success anyway.
4. **A judgment call** — only when the transform needs a decision a
   pattern cannot express (which type should own this behaviour).
5. **Never `sed`.** It rewrites strings and comments indiscriminately. The
   tell: after a pure rename, `git diff` shows changes inside `//`, `///`,
   or a string literal.

Reviewer question for any large diff: *could this have been one
`ast-grep --update-all`?* If yes and it was not, reject it — a hand-made
version of a mechanical transform has hand-made mistakes in it.

## Free-Function Cluster → Type

**Shape:** three or more functions in a module threading the same leading
parameter pair or triple (ARCH-01).

1. Name the type after what the shared parameters *are together* — the
   context they form, not the module. `ClosureWalker`, not `CommonUtils`.
2. Create it with those values as private fields and one constructor.
3. Move functions in **one at a time**, each its own commit: the body
   travels verbatim, the shared parameters become `&self` access.
4. Functions in the cluster that do *not* take the shared parameters stay
   free. A cluster is rarely 100% one thing.

**Hazard:** `fn f(cfg: &Config, p: &Path)` → `impl Config { fn f(&self, p: &Path) }`
changes borrow duration. Code that previously borrowed `cfg` briefly now
holds `&self` for the whole call, so a caller that mutated `cfg` in
between stops compiling — or worse, an implicit clone silently disappears
and behaviour changes. Every conversion needs a parity run, not a build.

## God Struct → Cooperating Types

**Shape:** one type with many inherent `impl` blocks spread across many
files (ARCH-03) — the method surface decomposed into files while the type
stayed whole. The file split is already the design; it just is not in the
type system.

1. **One new type per existing file's `impl` block** is the default
   mapping, but collapse aggressively: five to seven cooperating types is
   usually right where twenty files exist. Group by the state each cluster
   actually reads.
2. Determine each new type's fields from what its methods touch — the
   subset of the god struct's fields, not all of them. A new type that
   needs every field has not decomposed anything.
3. The original keeps one field per extracted type, or constructs them per
   call. Delegating methods stay as a shim until call sites move.
4. Move one cluster per commit, parity-run each. Remove the shims last.

**Hazard:** the extraction is visibility-widening by construction — the
fastest fix for a private-item error is `pub`, and it is permanent. Review
every newly-`pub` item individually, and prefer moving over exporting
(ARCH-15).

## Parallel Families → Trait

**Shape:** two families of functions differing only by a backend prefix
(`github_*`/`gitlab_*`, `local_*`/`remote_*`) with near-1:1 parallelism.
The one case where judgment beats a codemod: a pattern cannot decide which
behaviour belongs to which type.

1. Write the trait from the *intersection* of the two families' shapes,
   naming methods for the operation, not the backend.
2. Implement it twice, moving bodies verbatim.
3. Convert call sites — generic parameter first, `dyn` only where a type
   parameter would otherwise propagate through the composition root.
4. Delete the free functions in the final commit.

**Hazard:** a trait also stored as `dyn` cannot use `async fn` in its
definition. Decide dispatch *before* writing the trait, or you will be
rewriting it after the first `dyn` call site appears.

## Module → Crate

Before anything: **break the module cycles** — a crate split does not
compile while any `use` cycle exists, and discovering that mid-extraction
wastes the whole pass. Read only the rows whose path is inside the module
you are moving, plus rows elsewhere naming that module; a pair appearing
both ways is the cycle.

```bash
rg -o 'use crate::[a-z_]+' --type rust --glob '!external/**' . | sort -u
```

1. Create the crate; add it to `[workspace.members]` and
   `[workspace.dependencies]`.
2. Add `[lints] workspace = true` to the new manifest — workspace lints
   are **not** inherited implicitly, and a member without the opt-in
   silently gets none.
3. Move the files unchanged. Fix `crate::` paths to the new crate name
   with SSR, not by hand.
4. In the original crate, `pub use <new_crate>::…` at every old path so
   nothing else changes yet.
5. Only after the build is green: update call sites, then drop the
   re-exports.

**Hazard:** an extracted crate pulls its dependencies with it. A `-types`
crate that ends up depending on tokio or an HTTP client has failed its
only purpose — check with `cargo tree -p <crate> -e normal` before
declaring the extraction done.

## Crate → Workspace

Do this as a sequence of module→crate extractions, leaves first, not as
one move. Between each, the tree builds and the oracle passes.

Order: the leaf types crate first (fewest inbound edges), then the core,
then the adapters, and leave the binary last. Every step keeps the old
paths alive behind re-exports until the final cleanup.

Declare dependency versions, package metadata and lints once at the
workspace level. Cargo's crate namespace is flat regardless of directory
layout, so keep the directories flat too — nested crate directories drift.

## Hazards That Compile

Check every move commit for these. All four are green-build defects.

| Hazard | Why it survives the build |
|---|---|
| A stubbed body — `todo!()`, `unimplemented!()`, `Ok(Default::default())` | Compiles clean; panics or silently succeeds at runtime |
| A copied private item instead of an exported one | Two definitions compile; behaviour forks the first time one is edited |
| `debug_assert!` with a side effect | The body is compiled out in release, so tests and production diverge |
| `unwrap_or(expensive())` where `unwrap_or_else` was meant | Eager evaluation; a fallback that panics fires exactly in the case it was meant to cover |

Do not restructure toward "more abstraction". A restructure with no
measured defect behind it adds risk and removes nothing — see
[architecture.md](architecture.md), which a refactor pass violates more
often than ordinary feature work does, in the name of consistency.
