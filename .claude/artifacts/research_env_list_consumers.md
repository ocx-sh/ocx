# Research: Option-list env-var consumer semantics (for the `list` modifier type)

**Provenance:** worker-researcher (sonnet), 2026-08-05, commissioned by `/swarm-plan` for
`plan_env_list_type.md` / `adr_env_modifier_types.md` (resolves
[ocx-sh/ocx#277](https://github.com/ocx-sh/ocx/issues/277)). Verifies the load-bearing
design claim: *for option-list env vars, the downstream consumer resolves duplicates
last-wins, so ordered append alone gives override semantics.*

**Verdict:** claim holds for **JDK_JAVA_OPTIONS, NODE_OPTIONS (scalar flags), GODEBUG,
CFLAGS-by-convention, NO_PROXY** (set semantics — question doesn't apply). It **partially
breaks for RUST_LOG** (most-specific-target wins, not append order) and carries a **real
quoting landmine for NODE_OPTIONS** (no escaping mechanism exists — nodejs/node#21575).
None of it blocks the design; all of it scopes the documentation.

---

## 1. `JDK_JAVA_OPTIONS`

- **Separator**: whitespace (`isspace()`), like a shell command line.
- **Duplicate resolution**: last wins — inherited from HotSpot command-line parsing, not
  stated by the env-var spec itself. The spec's contract is only "prepend to command line,
  treated the same as command line arguments." Vendor consensus treats first-wins as a bug
  (an OpenJ9 report classifies first-wins `-Dfile.encoding` as a regression).
  - Source (official): "`JDK_JAVA_OPTIONS` prepends its content to the options parsed from
    the command line... treated in the same manner as that specified in the command line."
    — [java(1) man page, JDK 21][jdk-java-options]
- **Quoting grammar** (official, same page): single or double quotes enclose arguments
  containing whitespace; the pair is removed; an unmatched quote **aborts the launcher**.
  A real quote grammar — safer than naive space-joining *if* values are pre-quoted by the
  publisher. OCX's contribution-level dedup never parses elements, so quoted values pass
  through intact.
- **@argfile support**: yes, as on the command line (no wildcard).
- **Restrictions**: options that select the main class or exit early (`-jar`, `-h`, …) are
  disallowed — launcher aborts. Publisher responsibility; worth one doc line.
- **Side effect**: launcher prints a stderr notice whenever the var is set at all.
- **`JAVA_TOOL_OPTIONS`**: older JVMTI-oriented sibling, consumed at `JNI_CreateJavaVM`;
  disabled on some platforms when effective/real UID differ ([Oracle troubleshooting
  guide][java-tool-options]). **Ordering between the two vars + command line is not
  officially documented.** Best available evidence (empirical harness,
  [brunoborges/jdk-env-vars][jdk-env-vars]): `_JAVA_OPTIONS` > `JAVA_TOOL_OPTIONS` >
  `JDK_JAVA_OPTIONS`, command line beats all. **Unverified by official source — do not
  build on it.**

## 2. `NODE_OPTIONS`

- **Duplicate resolution — official**: "If an option that takes a single value ... is
  passed more than once, then the last passed value is used." — [Node.js CLI
  docs][node-options]. **Directly supports append-last-wins for scalar flags.**
- **Cumulative flags**: `--import`/`--require` stack in order, starting with NODE_OPTIONS —
  append is the *intended* semantics for these, by construction.
- **Precedence**: command line beats NODE_OPTIONS.
- **Quoting — the landmine**: NODE_OPTIONS has **no quoting/escaping mechanism at all**.
  [nodejs/node#21575][node-21575]: `NODE_OPTIONS="-r './test case.js'"` fails where the
  same argument works on the command line. Open, acknowledged limitation. **A list value
  containing whitespace silently breaks for the consumer, and no producer-side quoting can
  fix it.** → doc warning: NODE_OPTIONS list values must not contain whitespace.
- **Disallowed options**: per-option allowlist validated at parse; disallowed → immediate
  error (no exhaustive published list).

## 3. `GODEBUG`

- **Separator**: comma, `key=value` pairs.
- **Duplicate resolution — primary source (Go stdlib)**: last wins, deliberately: "Scan
  the string backward so that later settings are used and earlier settings are ignored."
  — [`internal/godebug` source][godebug-src]. **Directly supports the claim.**
- **Precedence** ([official][godebug-doc]): env var > `//go:debug` directives > `go.mod`
  `godebug` lines > toolchain defaults.
- Unrecognized settings are ignored, never an error. No quoting concerns.

## 4. `RUST_LOG` (env_logger / env_filter)

- **Separator**: comma; directives `target[=level]` or bare `level`.
- **Duplicate resolution — CONTRADICTS pure last-wins.** Verified from `env_filter`
  source: directives are **sorted by name length** ([filter.rs][env-filter-sort]), and
  matching iterates the sorted vector in reverse, returning on first prefix match
  ([directive.rs][env-filter-match]) — "Search for the longest match, the vector is
  assumed to be pre-sorted."
- **Effective rule: most-specific-target wins**, independent of append order.
  `hello::world=debug,hello=info` ≡ `hello=info,hello::world=debug`. Only equal-specificity
  duplicates tie-break by append order (stable sort + reverse iteration → later wins).
- **Impact**: append is *safe* (never destructive; a later more-specific directive always
  overrides its narrower target), but appending a **broader** directive cannot reset an
  earlier **narrower** one — `warn` appended after `my_crate::noisy=trace` does not silence
  the module. → docs must say "layer, most-specific-wins" for RUST_LOG, not "override".

## 5. `CFLAGS` / `LDFLAGS`

- Not a duplicate-key merge at all — opaque flag strings; "override" happens via GNU
  ordering conventions on the compile line.
- **User-flags-last convention (official)**: `$(CPPFLAGS)` appears after `$(AM_CPPFLAGS)`
  because "users should have the last say"; packages must never set user variables —
  [Automake: Flag Variables Ordering][automake-flags],
  [Autoconf: Preset Output Variables][autoconf-preset]. Append matches the ecosystem's own
  convention.
- **GCC duplicate handling**: `-D` same macro — last wins; `-D`/`-U` mix — `-U` wins
  regardless of order ([GCC Preprocessor Options][gcc-cpp]; page fetch was truncated,
  corroborated via indexed copies — high-but-not-verbatim confidence).
- **`-I`/`-L` are FIRST-wins**: directories searched "in left-to-right order" —
  [GCC Directory Options][gcc-dirs]. Appending a new `-I` adds a *fallback*, never an
  override. → doc note: reversed polarity for search-path flags.

## 6. `NO_PROXY`

- **Separator**: comma ([curl CURLOPT_NOPROXY][curl-noproxy]). Suffix/domain matching;
  `*` wildcard; CIDR supported.
- **Set semantics, not map semantics** — OR-match over patterns; duplicates are inert.
  "Does append override" is the wrong question; append gives correct *union* semantics
  trivially. Different consumer category from the five above.

## 7. Prior art for env merge operators

- **direnv**: `PATH_add`/`path_add` (prepend-only; repeated calls genuinely duplicate),
  `MANPATH_add`, `load_prefix`. **`env_default` does not exist** in current direnv
  (verified against [stdlib.sh][direnv-stdlib]) — earlier citations of it are stale. No
  append primitive, no configurable separator, no generic list concept.
- **systemd `Environment=`** ([systemd.exec(5)][systemd-exec]): "If the same variable is
  set twice, the later setting will override the earlier setting" — explicit last-wins;
  double-quote-only quoting, no `$` expansion; **reset-via-empty-assignment** clears all
  prior settings — clean prior art for a future "clear the list" need.
- **Nix `makeWrapper`** ([make-wrapper.sh][make-wrapper]) — nearest prior art,
  package-manager-shaped: `--set`, `--set-default` (a real named default operator),
  **`--prefix ENV SEP VAL` / `--suffix ENV SEP VAL` — caller-supplied separator**, exactly
  OCX's configurable-separator design. Dedup is asymmetric: prefix **removes existing
  occurrence then prepends** (move-to-front); suffix **appends only if absent**
  (skip-if-present — a later layer's identical value keeps its old, possibly losing,
  position). OCX's move-to-back is deliberately stronger; skip-if-present is the shape of
  rustup's documented precedence bug (see `adr_idempotent_path_move_to_front.md`).
- **mise**: `env._.path` is PATH-only prepend; layer-precedence config merging, no
  per-variable operator vocabulary. OCX's generic `list` is more general than direnv/mise;
  makeWrapper is the only comparable operator set.

## Summary table

| Var | Separator | Duplicate rule | Confirmed how | Breaks naive append-wins? |
|---|---|---|---|---|
| JDK_JAVA_OPTIONS | whitespace | last wins (JVM arg parsing) | vendor consensus; spec says "same as command line" | No; JAVA_TOOL_OPTIONS ordering undocumented |
| NODE_OPTIONS | whitespace | last wins (scalar); cumulative flags stack | official docs | **Quoting: no escape mechanism exists** ([#21575][node-21575]) |
| GODEBUG | comma | last wins, by design (backward scan) | Go stdlib source | No |
| RUST_LOG | comma | **most-specific-target wins** | env_filter source | **Yes** — broader-later can't reset narrower-earlier |
| CFLAGS/LDFLAGS | space | convention: user flags last; `-D` last-wins; **`-I`/`-L` first-wins** | GNU manuals | **Yes for `-I`/`-L`** |
| NO_PROXY | comma | N/A — set/union semantics | curl docs | No (question doesn't apply) |

## Recommendation

Ship `list`. Append is sound for 5/6 and is the only shape that expresses NODE_OPTIONS
`--import` stacking and NO_PROXY union correctly. Scope docs honestly: (1) RUST_LOG is
"layer, most-specific-wins", not "override"; (2) warn that NODE_OPTIONS values must not
contain whitespace (no downstream escape exists); (3) flag `-I`/`-L` reversed polarity if
`list` targets compiler-flag variables. Documentation precision, not semantics bugs.

<!-- sources -->
[jdk-java-options]: https://docs.oracle.com/en/java/javase/21/docs/specs/man/java.html
[java-tool-options]: https://docs.oracle.com/javase/8/docs/technotes/guides/troubleshoot/envvars002.html
[jdk-env-vars]: https://github.com/brunoborges/jdk-env-vars
[node-options]: https://nodejs.org/api/cli.html#node_optionsoptions
[node-21575]: https://github.com/nodejs/node/issues/21575
[godebug-doc]: https://go.dev/doc/godebug
[godebug-src]: https://docs.go101.org/std/src/internal/godebug/godebug.go.html
[env-filter-match]: https://docs.rs/env_filter/latest/src/env_filter/directive.rs.html
[env-filter-sort]: https://docs.rs/env_filter/latest/src/env_filter/filter.rs.html
[automake-flags]: https://www.gnu.org/software/automake/manual/html_node/Flag-Variables-Ordering.html
[autoconf-preset]: https://www.gnu.org/software/autoconf/manual/autoconf-2.72/html_node/Preset-Output-Variables.html
[gcc-cpp]: https://gcc.gnu.org/onlinedocs/gcc/Preprocessor-Options.html
[gcc-dirs]: https://gcc.gnu.org/onlinedocs/gcc/Directory-Options.html
[curl-noproxy]: https://curl.se/libcurl/c/CURLOPT_NOPROXY.html
[direnv-stdlib]: https://github.com/direnv/direnv/blob/master/stdlib.sh
[systemd-exec]: https://manpages.debian.org/unstable/systemd/systemd.exec.5.en.html
[make-wrapper]: https://github.com/NixOS/nixpkgs/blob/master/pkgs/build-support/setup-hooks/make-wrapper.sh
