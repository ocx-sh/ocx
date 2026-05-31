# Research: Robust libc Host Detection (discovery-then-identify)

**Date:** 2026-05-31
**Author:** builder (via /swarm-execute libc detection robustness)
**Purpose:** Make `HostCapabilities::detect()` robust on non-FHS hosts (NixOS,
Gentoo Prefix, Homebrew-on-Linux, conda, custom sysroots, Bazel hermetic
sandbox) where the dynamic loader is not at a canonical FHS path. Feeds the
"Detection mechanism v2" amendment to `adr_platform_libc_os_features.md`.
Follow-on to `research_libc_detection_methods.md` (which fixed the
first-wins / single-value bugs and chose disk enumeration).

---

## Headline

The v1 detection **model** is sound: enumerate loaders on disk, probe each with
`--version`, set-union the positives into a `BTreeSet<LibcFlavor>` (NOT
self-linkage via `/proc/self/maps` or `dl_iterate_phdr`). The **weak spot** is
the hardcoded FHS path allowlist (`GLIBC_LOADERS` / `MUSL_LOADERS`): it
false-negatives wherever the loader is not at a canonical FHS path. The set
ends up empty → `os_features` empty → `Any`-only matching, losing libc-aware
auto-selection precisely in the container/virtualized environments OCX targets.

SOTA (Node [`detect-libc`][detect-libc], Python [PEP 656][pep656] musllinux)
does **not** hardcode paths. It **discovers** the loader by reading `PT_INTERP`
from a present system binary — the embedded string IS the host's exact loader
path — then **identifies** it by banner. We adopt discovery-then-identify, keep
the allowlist as a last-resort fallback, and make family classification
table-driven so a third libc family is a one-row addition.

**Decision: three-source discovery (PT_INTERP ∪ arch-filtered directory scan ∪
hardcoded allowlist), deduped by canonical path, then banner-only
identification.**

---

## Why the FHS allowlist false-negatives

The v1 allowlist enumerates fixed paths like `/lib64/ld-linux-x86-64.so.2`,
`/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2`, `/lib/ld-musl-x86_64.so.1`. These
cover stock Debian/Ubuntu/Fedora/Alpine. They miss every host that relocates the
loader:

| Host | Where the loader actually lives | v1 result |
|---|---|---|
| NixOS (no nix-ld) | `/nix/store/<hash>-glibc-*/lib/ld-linux-x86-64.so.2` | empty (false negative) |
| Gentoo Prefix | `$EPREFIX/lib*/ld-linux-*.so.*` | empty |
| Homebrew-on-Linux | inside the brew cellar / `glibc` formula prefix | empty |
| conda / micromamba env | `$CONDA_PREFIX/...` patched-ELF loaders | empty |
| custom sysroot / chroot | wherever the sysroot mounts it | empty |
| Bazel hermetic sandbox | sandboxed lib tree | empty |
| future distro that moves the loader | new canonical path | empty until allowlist patched |

All degrade *gracefully* (empty set → `Any`-only matching, `--platform`
override exists), so this is an enhancement, not a firefight. But these are
exactly the CI/virtualized environments in OCX's target.

---

## Discovery mechanism comparison

| Mechanism | What it yields | Finds loader on non-FHS host? | Multi-libc | Subprocess | Verdict |
|---|---|---|---|---|---|
| **`PT_INTERP` of a system binary** (chosen, primary) | the host's exact native loader path | **yes** (the path is embedded in the binary, wherever it points) | one per binary read | no (ELF parse) | **best discovery signal.** Static busybox has none → fall through. |
| **Arch-filtered directory scan** (chosen, secondary) | loader files present in canonical dirs | only at scanned dirs | **yes** (finds every loader in the dirs) | no (readdir) | catches multi-libc hosts the single PT_INTERP read misses; arch filter rejects foreign-arch multiarch loaders |
| **Hardcoded FHS allowlist** (chosen, fallback) | fixed canonical paths | no | yes | no (stat) | last resort; the v1 mechanism, demoted |
| `ldconfig -p` | glibc loader cache | no (cache, not store-relative on Nix); **absent on musl/Alpine** | glibc only | yes | rejected — musl loaders never registered; absent on Alpine |
| `getconf GNU_LIBC_VERSION` | glibc *version* | n/a | glibc only | yes | future `os.version` glibc-version signal, not family detection |
| `/etc/os-release` `ID` | distro identity | n/a (identity, not capability) | no | no | **rejected as a signal** — Chimera Linux is musl with a glibc-looking userland; Void ships both; gcompat hosts are `ID=alpine` but can launch glibc. Distro ≠ loader. |
| `/proc/self/maps`, `dl_iterate_phdr` | ocx's OWN linkage | wrong question | never | no | rejected (per v1 research — self ≠ host for a static tool) |

### PT_INTERP mechanics

`PT_INTERP` (program-header type 3) is the NUL-terminated path of the dynamic
loader the kernel `execve`s for that binary. It is embedded at link time and
points at the loader on **the host the binary was built for / runs on** — on
NixOS that is the `/nix/store` path, on Gentoo Prefix the prefix path, on
stock Ubuntu `/lib64/ld-linux-x86-64.so.2`. Reading it from a guaranteed-present
system binary (`/usr/bin/env`, `/bin/sh`, `/bin/ls`) yields the host's native
loader with zero path guessing. A statically linked binary (busybox on minimal
Alpine) has no `PT_INTERP` segment → skip it and fall through to the next probe
binary, then to the scan, then to the allowlist.

Ordered allowlist of probe binaries is fixed (never user input) for the same
reason the loader allowlist is: the discovered loader is later spawned.

---

## Identification: banner-only, table-driven

Identification classifies each discovered loader **purely by its `--version`
banner**, independent of which source produced the path:

- musl: stdout/stderr contains `"musl libc"` → `Musl` (exit status ignored —
  musl's loader exits non-zero on `--version` by design).
- glibc: contains `"GNU libc"` / `"GLIBC"` → `Glibc`; else, if the loader name
  looks glibc (`ld-linux-*`) AND it exited 127 (Ubuntu 20.04 / glibc 2.31
  quirk), confirm with `{loader} /bin/true` and treat exit 0 as glibc.

**gcompat falls out for free.** The Alpine gcompat stub sits at the glibc loader
path but prints the **musl** banner → classified `Musl`. The ADR "identity, not
equivalence" rule is preserved *by construction* — the v1 special-case exclusion
code disappears.

Families are expressed as a descriptor table (`flavor`, arch loader-name
fragments for the scan filter + glibc name heuristic, banner predicate). Adding
uClibc-ng / Bionic later = one row + its fragments. Not added now (YAGNI); the
`LibcFlavor::Unknown` variant already parses inbound unknown `libc.*` tags
losslessly, reserving the namespace.

---

## Virtualization / namespace failure modes

| Scenario | detect-env | exec-env | Does v2 detect correctly? |
|---|---|---|---|
| Stock Ubuntu/Fedora/Debian | glibc | glibc | yes (PT_INTERP or scan) |
| Alpine pure musl | musl | musl | yes (PT_INTERP → `/lib/ld-musl-*`) |
| Alpine + gcompat | musl (loader identity) | musl | yes — reports musl only (banner rule) |
| Ubuntu + `musl-tools` | {glibc, musl} | both | yes — scan finds both loaders |
| NixOS (no nix-ld) | glibc via `/nix/store` PT_INTERP | glibc | **yes (v2 fix)** — v1 returned empty |
| Gentoo Prefix | glibc via prefix PT_INTERP | glibc | **yes (v2 fix)** |
| NixOS minimal (static probe binaries, no /nix loader readable) | empty | — | empty → `Any`-only + `/nix` debug note (unchanged graceful degrade) |
| **distrobox / toolbox** (detect on host, run in guest) | host libc | guest libc | **NO — documented limitation** |
| **bind-mounted container** (install here, run in container ns) | host libc | container libc | **NO — documented limitation** |
| **install-here-run-there** (copy `~/.ocx` to another host) | build host libc | run host libc | **NO — documented limitation** |

The bottom three are the **detect-env ≠ exec-env gap**: detection answers "what
can *this host* run?", but the binary may ultimately run in a different
namespace with a different libc set. OCX's normal `ocx exec` runs on the same
host/kernel, so the gap does not bite the common path. Documented as a known
limitation; exec-time / target-namespace detection is deferred to a separate
ADR.

---

## Dependency decision: `elf` crate, not `goblin`

PT_INTERP needs an ELF program-header reader. Two candidates:

| Axis | `elf` (cole14/rust-elf) | `goblin` |
|---|---|---|
| Runtime deps | **zero** | scroll + plain + log (none in tree) |
| `unsafe` | **none** | uses unsafe zero-copy |
| License | MIT OR Apache-2.0 (in `deny.toml`) | MIT (fine, but no Apache dual) |
| Scope | **ELF only** (ISP) | ELF + Mach-O + PE + archive (dead weight here) |
| PT_INTERP ergonomics | ~6 lines of segment iteration | `.interpreter` one-liner |
| Version pinned | `0.7.4` (zero transitive deps confirmed via `cargo tree -i elf`) | — |

**Decision: `elf`.** Zero runtime deps, zero `unsafe`, dual-license match, and
ELF-only scope (we never parse Mach-O/PE here) outweigh goblin's one-liner
convenience. The ~6 lines of `segments()` iteration to find `PT_INTERP` and read
`data[p_offset .. p_offset+p_filesz]` (NUL-trimmed) is the entire cost. API
used: `ElfBytes::<AnyEndian>::minimal_parse` → `.segments()` →
`ProgramHeader { p_type, p_offset, p_filesz }` vs `abi::PT_INTERP`.

---

## Recommendation (implemented)

1. **Discover** a deduped candidate-loader set from three sources, in order:
   PT_INTERP of `INTERP_PROBE_BINARIES` (first hit wins, break) ∪ arch-filtered
   scan of `/lib`, `/lib64`, `/usr/lib`, `/usr/lib64` + immediate multiarch
   subdirs ∪ hardcoded `GLIBC_LOADERS`/`MUSL_LOADERS` fallback. Dedup by
   canonical path via `dedup_unseen` before any spawn.
2. **Identify** each loader by `--version` banner only, table-driven over
   `LIBC_FAMILIES`, preserving the musl exit-status-ignored and glibc exit-127 →
   `/bin/true` quirks. Banner precedence makes gcompat → musl automatic.
3. Union positives into the existing `BTreeSet<LibcFlavor>`. Keep
   `PROBE_TIMEOUT`, the concurrent `JoinSet`, the non-Linux empty-set
   short-circuit, the `__OCX_TEST_LIBC` seam, and the NixOS `/nix` debug note.
4. Emit glibc + musl only this round. Reserve `libc.*` namespace via
   `Unknown`. uClibc-ng / Bionic not actively probed (YAGNI).
5. Document detect-env ≠ exec-env as a known limitation; defer to a future ADR.

---

## Product context

No `product-context.md` shift. Differentiator #9 ("Automatic libc/ABI
resolution … selected per-host from one OCI index") still holds and is
*strengthened*: it now resolves correctly on non-FHS hosts (NixOS, Gentoo
Prefix) that the FHS-only allowlist missed.

---

## Sources

- lovell/detect-libc (Node reference, PT_INTERP / report-based) — https://github.com/lovell/detect-libc
- PEP 656 (musllinux) — https://peps.python.org/pep-0656/
- PEP 600 (manylinux, glibc subset/superset) — https://peps.python.org/pep-0600/
- cargo-binstall `detect-targets` (banner mechanics reference) — https://github.com/cargo-bins/cargo-binstall/tree/main/crates/detect-targets
- `elf` crate (cole14/rust-elf) — https://docs.rs/elf/0.7.4 ; https://github.com/cole14/rust-elf
- `goblin` crate — https://docs.rs/goblin
- ELF `PT_INTERP` / program headers — https://refspecs.linuxfoundation.org/elf/elf.pdf ; `man 5 elf`
- NixOS nix-ld — https://wiki.nixos.org/wiki/Nix-ld
- Gentoo Prefix — https://wiki.gentoo.org/wiki/Project:Prefix
- Chimera Linux (musl, non-Alpine) — https://chimera-linux.org/
- gcompat (Alpine glibc compat) — https://gitlab.alpinelinux.org/alpine/gcompat

[detect-libc]: https://github.com/lovell/detect-libc
[pep656]: https://peps.python.org/pep-0656/
