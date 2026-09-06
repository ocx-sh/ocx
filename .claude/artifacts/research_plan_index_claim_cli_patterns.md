# Research: CLI deprecation window and shared option surfaces

**Date:** 2026-09-05
**Expires:** 2027-03-05
**Axis:** design patterns
**Consumer:** plan_index_claim_command.md

## Summary

- The project's `deprecated.rs` mechanism is **subcommand-shaped only**: a hidden `enum` variant + a dispatch-arm call to `warn_renamed`. It has no field-level equivalent, so a `--package` → positional move on `PackageAnnounce` needs a new (but small) pattern, not a reuse of `deprecated.rs`.
- clap **cannot** merge one flag spelling and one positional spelling into a single `Arg`/id — confirmed by clap issue [#1820](https://github.com/clap-rs/clap/issues/1820), closed `wontfix`. The supported shape is **two distinct `Arg` ids**, merged in application code — exactly what [jj's `jj bisect run --command`](https://github.com/jj-vcs/jj/blob/main/cli/src/commands/bisect/run.rs) deprecation does.
- Because the two spellings are two different ids, **`ArgMatches::value_source` is not the detection mechanism** — `Option::is_some()` on the hidden flag's own field already tells you it was used. `value_source` only distinguishes *command-line vs env vs default vs config*, not *typed as `--flag` vs typed bare* — those degenerate to the same id only if clap allowed arg-sharing, which it does not.
- `conflicts_with`/`conflicts_with_all` is registered symmetrically once, from either side — the project's own `package_announce.rs` already relies on this (the `tags` field lists all three siblings; the siblings do not list `tags` back).
- `required_unless_present_any` + `conflicts_with_all`, exactly as used today for the `tags`/`tags_file`/`tags_from_registry`/`refresh` group in `package_announce.rs`, is the declarative shape for "exactly one of N mutually exclusive inputs, one of them hidden."
- Real warn-once precedent in a **clap-based** CLI, deprecating a flag for a positional, on stderr, with a regression test asserting the stderr text: jj's `jj bisect run --command` → positional `COMMAND` (`cli/src/commands/bisect/run.rs`, tested in `cli/tests/test_bisect_command.rs`).
- Negative real-world counter-example worth citing: GitHub CLI shipped a deprecation warning **on stdout**, breaking `--jq`-piped scripts, filed as [cli/cli#5674](https://github.com/cli/cli/issues/5674) and fixed in [cli/cli#5698](https://github.com/cli/cli/pull/5698) by rerouting Cobra's `SetOut` to the same stream as `SetErr`. Same failure mode `deprecated.rs` already avoids by routing through `Context::ui().warn`.
- The repo's `crates/ocx_cli/src/options/` module (not `command/options/` — that path doesn't exist) is the existing flatten-struct convention; `LazyMode` (`options/lazy_mode.rs`) is a complete, minimal worked example.
- The repo has **no** `get_arguments()`/`ArgGroup`/`value_source`-based introspection test today. Real precedent for exactly this parity-test shape exists in `foundry-rs/foundry`: `crates/cli/src/opts/rpc.rs` and `crates/cli/src/opts/evm.rs` both run the identical `command.get_arguments().find(|arg| arg.get_id() == "rpc_url")` regression test, because both structs flatten the same shared `RpcCommonOpts`.
- `crates/ocx_cli/src/options/lazy_mode.rs` (the file named in the task as a possible existing parity suite) is a single-flag flatten struct with its own unit tests — it is not a cross-command parity suite. There is nothing to reuse there beyond the pattern.

## Findings

### 1. The project's own precedent (`deprecated.rs`) — and its gap for flag→positional

**CLAUDE.md, "Batched-window carve-out"** (quoted in full):

> A rename reaching dozens of files across docs, tests and downstream repos may ship one deprecation window instead of a hard break, on these terms: the old spelling stays as a *hidden* command — a clap alias is undetectable at parse time, so a warning needs its own hidden variant — it warns once on stderr and never on stdout, its removal release is named when the window opens, and every old spelling in flight lives in one `deprecated.rs` deleted whole. One window per release pair, not one per rename; still no migration prose in user docs. In flight: `ocx run` → `ocx exec`, deprecated in 0.6, removed in 0.7.

**`crates/ocx_cli/src/command/deprecated.rs` (quoted in full, 33 lines):**

```rust
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deprecated command spellings, kept alive for exactly one release pair.
//!
//! Every name renamed in 0.6 dispatches through this module, so one grep finds
//! the whole set and 0.7 removes it by deleting this file together with the
//! hidden `Command` / `Package` variants that call it. Nothing else may depend
//! on it.
//!
//! An old spelling is a *hidden command*, never a clap alias: `ArgMatches`
//! reports the canonical name, so an alias is invisible to the code and could
//! not warn. The hidden variant also keeps its own
//! [`crate::app::canonical_command_name`] arm reporting the **old** string, so
//! the frozen v1 error envelope still distinguishes a deprecated invocation
//! from a current one and nothing already emitted by a released binary changes
//! meaning.

use crate::app::Context;

/// The release that deletes this module and every spelling in it.
const REMOVAL_RELEASE: &str = "0.7";

/// Warn on stderr that `old` has been renamed to `new`.
///
/// Fires once per invocation by construction — one process dispatches one
/// command. Routed through [`Context::ui`], so the notice never reaches stdout
/// and degrades to `log::warn!` when quiet or non-interactive.
pub fn warn_renamed(context: &Context, old: &str, new: &str) {
    context.ui().warn(format!(
        "`ocx {old}` is renamed to `ocx {new}` and is removed in {REMOVAL_RELEASE}"
    ));
}
```

**The mechanism in use** (`crates/ocx_cli/src/command/package.rs`), quoted verbatim:

```rust
    /// Deprecated spelling of `ocx package description push`; removed in 0.7.
    #[command(name = "describe", hide = true)]
    DeprecatedDescribe(super::package_description_push::PackageDescriptionPush),
    /// Deprecated spelling of `ocx package description pull`; removed in 0.7.
    #[command(name = "info", hide = true)]
    DeprecatedInfo(super::package_description_pull::PackageDescriptionPull),
```

and the dispatch arms:

```rust
            Package::DeprecatedDescribe(push) => {
                super::deprecated::warn_renamed(&context, "package describe", "package description push");
                push.execute(context).await
            }
            Package::DeprecatedInfo(pull) => {
                super::deprecated::warn_renamed(&context, "package info", "package description pull");
                pull.execute(context).await
            }
```

and the JSON-envelope command-name mapping that keeps the **old string** alive for the frozen v1 envelope (`crates/ocx_cli/src/app.rs`):

```rust
            // Old spellings keep their released strings — see the note on
            // `Command::DeprecatedRun` above.
            PackageCmd::DeprecatedDescribe(_) => "package describe",
            PackageCmd::DeprecatedInfo(_) => "package info",
```

**Is there a test?** No. `grep -rn "deprecated"` across `crates/ocx_cli/src/command/` turns up only `deprecated.rs` itself and the two `package.rs` call sites quoted above — no `#[test]` exercises `warn_renamed`, the hidden-variant dispatch, or the stderr-only guarantee. This is a **negative finding**: the project's own mechanism is untested. (Relevant if the plan wants to add a regression test for the new flag→positional warning — there is no existing test to model it on in-repo; model it on jj's instead, Finding 4.)

**Does it cover flag→positional?** No — and precisely, not "not directly," but **structurally cannot**, as designed:

- The unit of duplication is a whole `Subcommand` enum variant (`Package::DeprecatedDescribe` wraps the *same args struct* as the new command, `PackageDescriptionPush`). Dispatch is a `match` arm on the *enum*, not on any field inside an args struct.
- `warn_renamed(old, new)` takes two **command-name strings** (`"package describe"`, `"package description push"`) — there is no equivalent for "flag `--package` on this same command was used instead of the positional."
- The doc comment's premise — "a clap alias is undetectable at parse time" — is the *general* clap fact (confirmed independently in Finding 2), and it does generalize to flags: a `long = "package"` `Arg` also collapses to one id regardless of spelling used. But the *fix* the module ships (hidden enum variant) only works because a whole subcommand can be trivially duplicated with its own `Command::name`. An `Arg` cannot be "duplicated with a different id but the same underlying storage" the way a `Subcommand` variant can — clap subcommands are keyed by `Command::name`/`alias`, while args are keyed by a single `Id` per field. So the *pattern* (hide + duplicate + detect + warn) carries over, but the *unit* it operates on does not — it has to move from "enum variant" to "one extra hidden `Arg` on the existing struct."
- `warn_renamed` itself is fully reusable as-is: nothing in its signature or behavior is subcommand-specific. The plan should call `super::deprecated::warn_renamed(&context, "package announce --package", "package announce <IDENTIFIER>")` from inside `PackageAnnounce::execute`, not from a `Package`-enum dispatch arm.

**Gap, stated precisely:** `deprecated.rs` gives you the warning primitive and the "hidden, one file, deleted whole in 0.7" discipline; it does not give you flag-vs-positional detection, because the whole file's design axis is "which enum variant matched," and this migration's design axis is "which of two `Arg`s on the same variant was populated."

### 2. clap mechanics for a flag→positional window

**The critical sub-question first, because it changes the whole shape of the answer.**

CLAUDE.md's premise is "a clap alias is undetectable at parse time." Verified, and it goes further than the plan may expect:

- clap's own doc comment on `Command::alias` (fetched from `clap_builder/src/builder/command.rs`, `clap-rs/clap@master`) states outright:
  > "**NOTE:** When using aliases and checking for the existence of a particular subcommand within an [`ArgMatches`] struct, one only needs to search for the original name and not all aliases."

  i.e. `ArgMatches` normalizes to the canonical name; the alias string the user actually typed is not recoverable. Confirms the project's claim for **subcommand** aliases exactly as stated.
- The same limitation exists one level down, for a single `Arg` having **both** a flag spelling and a positional spelling: clap issue [clap-rs/clap#1820, "Allow accepting an argument by position and by flag"](https://github.com/clap-rs/clap/issues/1820) asked for exactly this (an `Arg::positional(bool)` toggle so a flag could also be typed bare) and was **closed `wontfix`**. `Arg::index()`'s own doc is explicit: *"This is only meant to be used for positional arguments and shouldn't be used with `Arg::short` or `Arg::long`."* So clap does not merely fail to expose which spelling was used — it refuses to let one `Arg`/id carry both spellings at all.
- Consequence: **`ArgMatches::value_source` is the wrong tool for this job.** `value_source(id)` (docs.rs, `clap::ArgMatches::value_source`) distinguishes `ValueSource::CommandLine` from `EnvVariable` / `DefaultValue`. If `--package` and the positional were (hypothetically) the same id, both would report `CommandLine` — value_source cannot tell you *which syntax* produced the command-line value, because clap never lets that ambiguity exist on one id in the first place. Since it can't be one id, the whole premise for needing `value_source` disappears: the flag and the positional are always two different fields, and checking `Option::is_some()` on the flag's own field *is* the detection.

**The supported shape — two distinct `Arg`s, merged in code.** This is exactly what `jj` does today for its own flag→positional migration (`jj-vcs/jj`, `cli/src/commands/bisect/run.rs`):

```rust
/// Deprecated. Use positional arguments instead.
#[arg(
    long = "command",
    value_name = "COMMAND",
    hide = true,
    conflicts_with = "command"
)]
legacy_command: Option<CommandNameAndArgs>,

/// Command to run to determine the status of a revision
#[arg(value_name = "COMMAND")]
command: Option<String>,
```

and the merge/detect/warn logic in `cmd_bisect_run`:

```rust
if let Some(command) = &args.legacy_command {
    writeln!(
        ui.warning_default(),
        "`--command` is deprecated; use positional arguments instead: `jj bisect run \
         --range=... -- {command}`"
    )?;
} else if args.command.is_none() {
    return Err(cli_error("Command argument is required"));
}
```

and the regression test asserting the warning lands on **stderr** with the exact text (`cli/tests/test_bisect_command.rs`):

```rust
insta::assert_snapshot!(work_dir.run_jj(["bisect", "run", "--range=..", "--command", &bisector_path]).success().stderr, @"
Warning: `--command` is deprecated; use positional arguments instead: `jj bisect run --range=... -- $FAKE_BISECTOR_PATH`
...
```

**Mapping the four requirements onto this repo's conventions, using the announce case:**

- **(a) hide the flag from `--help`:** `#[clap(long = "package", hide = true)]` on the old field. (`hide` is a plain `Arg` attribute — also used this way in `uv`'s own deprecated global flag, `astral-sh/uv`, `crates/uv-cli/src/lib.rs`: `#[arg(global = true, long, hide = true, value_parser = clap::builder::BoolishValueParser::new())]` above a doc comment reading "This option is deprecated in favor of `--no-config`.")
- **(b) mutually exclusive with a good error:** `conflicts_with = "package_positional"` on the flag field (or the reverse — see symmetry note below). clap's usage error for a declared conflict is generated automatically and does not need a hand-written message.
- **(c) requiring exactly one of them:** declaratively, the same shape `package_announce.rs` already uses today for `tags`/`tags_file`/`tags_from_registry`/`refresh` — `required_unless_present_any` on one side, naming the sibling(s). Quoted from the file, verbatim:

  ```rust
  #[clap(
      long = "tags",
      value_name = "TAGS",
      value_delimiter = ',',
      conflicts_with_all = TAG_SELECTION_SIBLINGS_OF_TAGS,
      required_unless_present_any = TAG_SELECTION_SIBLINGS_OF_TAGS,
  )]
  tags: Vec<String>,
  ```

  For flag→positional this becomes (illustrative, not the plan's final field names):

  ```rust
  /// Package to announce, as `<namespace>/<package>`.
  ///
  /// Deprecated spelling of the positional form below; removed in 0.7.
  #[clap(long = "package", hide = true, conflicts_with = "package")]
  package_flag: Option<options::Identifier>,

  /// Package to announce, as `<namespace>/<package>` (e.g. `acme/widget`).
  #[clap(required_unless_present = "package_flag")]
  package: Option<options::Identifier>,
  ```

  (Field name collision note: the positional field is literally named `package` today, so the hidden flag field needs a different Rust identifier — `package_flag` — while keeping `long = "package"` as its clap spelling; `conflicts_with`/`required_unless_present` refer to the *field*'s clap id, which defaults to the Rust identifier, i.e. `"package"` for the positional field.)

  If the auto-generated *combined* usage message (naming both alternatives, e.g. clap's `<--package|IDENTIFIER>` style output) is wanted instead of two independent single-arg rules, wrap both in an `ArgGroup` at the struct level — this repo already does exactly that for a different exclusivity rule in `package_push.rs`:

  ```rust
  #[clap(group(clap::ArgGroup::new("signing_target").args(["sign", "sbom"]).multiple(true)))]
  ```

  `ArgGroup::new("package_source").args(["package_flag", "package"]).required(true)` (no `.multiple(true)`) gives "exactly one of" with clap's own combined error text.

- **(d) detecting which one fired, to warn only for the flag:** `self.package_flag.is_some()` — no `value_source`, no `ArgGroup::id()` lookup needed, because the two are already separate fields. Merge with `self.package_flag.clone().or_else(|| self.package.clone())` before calling `.with_domain(...)`.

**Citations:** [`ArgMatches::value_source`, docs.rs](https://docs.rs/clap/latest/clap/struct.ArgMatches.html) · [`Arg::index`, docs.rs](https://docs.rs/clap/latest/clap/struct.Arg.html#method.index) · [clap-rs/clap#1820 "Allow accepting an argument by position and by flag" (closed wontfix)](https://github.com/clap-rs/clap/issues/1820) · [`Command::alias` doc comment, clap_builder/src/builder/command.rs](https://github.com/clap-rs/clap/blob/master/clap_builder/src/builder/command.rs) · [jj `cli/src/commands/bisect/run.rs`](https://github.com/jj-vcs/jj/blob/main/cli/src/commands/bisect/run.rs) · [jj `cli/tests/test_bisect_command.rs`](https://github.com/jj-vcs/jj/blob/main/cli/tests/test_bisect_command.rs) · [uv `crates/uv-cli/src/lib.rs`](https://github.com/astral-sh/uv/blob/main/crates/uv-cli/src/lib.rs) · in-repo: `crates/ocx_cli/src/command/package_announce.rs`, `crates/ocx_cli/src/command/package_push.rs`.

### 3. Warn-once discipline in comparable Rust CLIs

Two concrete examples, one positive and one negative (the negative is more instructive):

1. **jj (positive, and closest analog to this migration):** the exact `--command`-to-positional deprecation in Finding 2 emits its warning via `writeln!(ui.warning_default(), ...)`, which is jj's UI abstraction routed to stderr — structurally identical to this repo's `context.ui().warn(...)`. Its own test asserts the text lands in `.stderr`, not `.stdout`, using an insta snapshot (`cli/tests/test_bisect_command.rs`, quoted in Finding 2). This is the one piece of prior art in this research answering "how do you *test* a warn-once guarantee" — capture the child process's stdout and stderr separately and snapshot/assert against stderr only, asserting nothing unexpected lands in stdout.
2. **GitHub CLI `gh` (negative — a real regression, useful as the failure mode to design against):** [cli/cli#5674, "Flag deprecation warning should be sent to stderr not stdout"](https://github.com/cli/cli/issues/5674). Reported trigger:
   ```
   go run ./cmd/gh repo list cli --public --json name --jq '.[].name'
   ```
   The `--public` flag's deprecation notice was captured by `--jq`, corrupting piped/scripted output — precisely the failure `deprecated.rs`'s doc comment calls out ("the notice never reaches stdout"). Fixed in [cli/cli#5698](https://github.com/cli/cli/pull/5698) by repointing Cobra's `cmd.SetOut(...)` to the same `ErrOut` stream as `cmd.SetErr(...)`:
   ```go
   // before
   cmd.SetOut(f.IOStreams.Out)
   cmd.SetErr(f.IOStreams.ErrOut)
   // after
   cmd.SetOut(f.IOStreams.ErrOut) // command usage summary and deprecation warnings
   cmd.SetErr(f.IOStreams.ErrOut) // error messages
   ```
   This is Go/Cobra, not clap, but the lesson transfers directly: a deprecation warning is not automatically safe just because the code *calls* something that looks like a "stderr helper" — it is safe only when the underlying I/O sink is actually stderr, and that has to be checked, not assumed. Worth a one-line note in the plan: assert the sink, don't just trust the call site (`Context::ui().warn` should be grepped/tested to confirm it never falls through to a stdout `Write` impl — no such test exists today, per Finding 1).

**Uv and rustup, searched and not found with a citable code example:** uv has a `hide = true` deprecated-flag *definition* (Finding 2) but no located warning-emission code path or test in the time available — negative finding. rustup's deprecation warnings (e.g. around `--force-non-host`) surfaced only as GitHub issues *about* warning text, not as a located source snippet with a stderr assertion — negative finding; not citable to the "quote the code" bar this task set.

### 4. Flag→positional migrations in the wild

Two real examples found, of different character — one small-scale/graceful (matches this project's carve-out), one large-scale/hard-break (useful as contrast, not as a template):

1. **jj — `jj bisect run --command <COMMAND>` → `jj bisect run -- COMMAND` (graceful, in-place, exactly the shape this plan needs).** Already quoted in full in Finding 2. Summary against the three specific sub-asks:
   - *Help text:* the flag keeps a one-line doc comment ("Deprecated. Use positional arguments instead.") but is `hide = true`, so it does not appear in `--help` at all — matching this project's carve-out ("the old spelling stays as a *hidden*… variant").
   - *Error when both given:* `conflicts_with = "command"` on the flag produces clap's standard conflict error; no hand-rolled message.
   - *Removal announcement:* not yet removed as of the fetched source — jj's changelog process (`docs.jj-vcs.dev/latest/changelog/`) is the analogous place a removal would be announced; no separate "removed" entry was found in the time available (negative finding — could not confirm jj's removal-announcement wording specifically, only the deprecation-introduction wording).
2. **Helm 2 → 3 — `helm install --name NAME CHART` → `helm install NAME CHART` (hard break, cross-major-version, no in-binary warning).** [Helm's official migration guide](https://helm.sh/docs/v3/topics/v2_v3_migration/) documents the change directly: v2 took the release name via `--name`; v3 requires it as the first positional argument. There was **no soft deprecation window inside a single Helm binary** — v2 and v3 are different major-version binaries, and users who wanted v2-compatible `--name` filed [helm/helm#7345, "install: reintroduce name long flag (--name) for v2 compatibility"](https://github.com/helm/helm/issues/7345) *after the fact*, asking Helm to "honor name passed in as both an argument or a long option" — i.e. asking, post hoc, for the exact two-spelling-coexistence shape jj and this plan build proactively. This is a useful negative example for the plan to cite: shipping the hard break without a coexistence window generated a real user-filed request for the thing a graceful window would have provided for free. (Helm uses Cobra/pflag, not clap — cited for the migration-shape precedent and its help/error/announcement treatment, not for Rust/clap API detail.)

### 5. Shared options struct across sibling clap commands

**Canonical clap mechanism:** `#[command(flatten)]` (derive attribute, also written `#[clap(flatten)]` in this repo's style) on a field whose type derives `clap::Args`. Any struct implementing `Args` can be embedded into multiple `Parser`/`Args` structs this way, and clap merges its `Arg`s into the parent `Command` at `Command::build` time.

**Pitfalls, each with a citation:**

- **Help ordering / heading leakage.** A parent's `#[command(help_heading = "...")]` used to bleed into a flattened child's args, putting them under the wrong heading — filed as [clap-rs/clap#2803, "Don't leak `help_heading` when flattening"](https://github.com/clap-rs/clap/issues/2803), fixed in clap-rs/clap#2895. Net effect for a plan: don't assume a flattened struct's help heading is independent of where you flatten it in on old clap; on current clap (this repo pins `>=4.5.57, <5`) the fix has landed, but it is exactly the kind of drift a "shared struct, flattened into two commands" design has to watch for if the two consuming commands set different headings around the flatten point.
- **`Option<FlattenedStruct>` does not make the inner fields optional.** [clap-rs/clap#5092, "`#[command(flatten)]` on optional field makes it required"](https://github.com/clap-rs/clap/issues/5092), still open ("waiting on design," breaking-change label) as of the fetched state. Reproduction:
  ```rust
  #[command(flatten)]
  pub specific_args: Option<SpecificArgs>,
  ```
  does **not** make `SpecificArgs`'s own required fields optional — clap still demands them unless every field inside is independently `Option`/has a default. Relevant if the plan ever considers flattening the shared forge/transport struct as `Option<Shared>` to make the whole group optional on one sibling — it will not do what it looks like it does; each field inside the shared struct must carry its own optionality.
- **`requires`/`conflicts_with` across the flatten boundary work by `Id`, not by struct.** Since flattening just merges `Arg`s into one flat `Command` by id, a `requires = "some_field_in_the_other_struct"` from inside a flattened struct works exactly like same-struct `requires` — but only as long as the *id string* is unique across the whole merged command. Two flattened structs both defining a field with the same identifier will collide silently at the `Command` level (last one registered wins, or clap panics on debug-assert builds) — this repo's `options.rs` module comment already reasons about this exact class of collision risk (see the `key`/`rekor_upload`/`signature_format` `pub mod` note: "two of them editing one options file is the collision this layout exists to prevent").

**In-repo precedent — `crates/ocx_cli/src/options/lazy_mode.rs`, quoted (struct + its own doc comment):**

```rust
/// The `--lazy-mode` tier of the lazy-loading resolution ladder.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--lazy-mode <MODE>`.
/// Deliberately *not* a `--X`/`--no-X` pair (the `options::Pull` /
/// `options::BinScan` shape): a paired toggle can only express a closed two-
/// or three-valued set, and this mode is an open-ended strategy enum. Resolve
/// through [`LazyMode::mode`] — never read the field at a call site.
///
/// What [`LazyMode::mode`] returns is the ladder's **top tier**, not the
/// answer: `None` means "the flag was absent", which is what lets `ocx.toml`
/// and `OCX_LAZY_MODE` speak. Feed it to [`lazy::LazyModeLadder::cli`] and
/// call `resolve()`; the floor lives there and nowhere else.
///
/// See https://ocx.sh/docs/reference/command-line#arg-lazy-mode for the
/// full resolution order.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct LazyMode {
    /// Control when a tool's content downloads: now, or on first use.
    ///
    /// ...
    #[clap(long = "lazy-mode", value_enum, value_name = "MODE")]
    lazy_mode: Option<lazy::LazyMode>,
}
```

**The convention, as `options.rs` documents it:** each flattenable option group is its own module under `crates/ocx_cli/src/options/`, re-exported from `options.rs` by name (`pub use lazy_mode::LazyMode;`), consumed as `options::LazyMode` and attached with `#[clap(flatten)]`. A group **resolves through a method, never a bare field read** (`LazyMode::mode()`, not `self.lazy_mode`) — this is the load-bearing convention for a shared group: the struct owns its own resolution logic (env fallback, config fallback, default), so two commands that flatten it can never independently reimplement — and silently diverge on — that resolution order. `pub mod` vs `mod` + `pub use` is a second, narrower convention: `pub mod` is used only when either (a) there's no consumer yet and a `pub use` would trip `unused_imports` under `deny(warnings)`, or (b) two independent efforts need to reach the module without both editing `options.rs` (explicit rationale quoted in the file: "two of them editing one options file is the collision this layout exists to prevent"). For the announce/claim shared surface (`--index-repo`, `--forge`, `--fork`, `--out`, `--transport`), the plan should mint one new module (e.g. `options/registry_target.rs`) following the `LazyMode` shape — one `#[derive(clap::Args)]` struct, flattened into both `PackageAnnounce` and the new `PackageClaim`, with any shared resolution logic (e.g. inferring `--forge` from `--index-repo`'s host) living on the struct, not duplicated in both commands' `execute()`.

**Citations:** [clap derive `flatten`, clap_derive reference (docs.rs)](https://docs.rs/clap/latest/clap/_derive/index.html) · [clap-rs/clap#2803](https://github.com/clap-rs/clap/issues/2803) · [clap-rs/clap#5092](https://github.com/clap-rs/clap/issues/5092) · in-repo: `crates/ocx_cli/src/options.rs`, `crates/ocx_cli/src/options/lazy_mode.rs`.

### 6. Testing the shared surface

**clap API for introspecting a built `Command`:** `Command::get_arguments(&self) -> impl Iterator<Item = &Arg>` (via the `CommandFactory` trait's `T::command()` for a derive `Parser`/`Args` struct), and `Arg::get_id(&self) -> &Id` (an `Id` compares equal to `&str` directly, so `arg.get_id() == "forge"` typechecks without an explicit `.as_str()`). This is a real, exercised API — not merely documented but used in production test suites.

**Concrete, real-world precedent for exactly this parity shape** — `foundry-rs/foundry` (Foundry, the widely-used Solidity toolchain, `forge`/`cast`/`anvil`), where **two sibling option structs both flatten the same shared struct** and both carry an identical regression test guarding one property of the shared flag:

`crates/cli/src/opts/rpc.rs`:
```rust
#[derive(Clone, Debug, Default, Parser)]
#[command(next_help_heading = "Rpc options")]
pub struct RpcOpts {
    #[command(flatten)]
    pub common: RpcCommonOpts,
    // ...
}

#[test]
fn rpc_url_arg_does_not_read_eth_rpc_url_env() {
    let command = RpcOpts::command();
    let rpc_url = command.get_arguments()
        .find(|arg| arg.get_id() == "rpc_url")
        .expect("rpc_url arg");
    assert!(rpc_url.get_env().is_none());
}
```

`crates/cli/src/opts/evm.rs` — `EvmArgs` independently flattens the same `RpcCommonOpts`, and carries the **identical** test, over the same `rpc_url` id. The two sibling structs share one source of truth for the flag (`RpcCommonOpts`), and the parity test's actual job is to catch the day one of the two siblings stops flattening the shared struct and starts redefining the field ad hoc — at which point the flag could silently diverge (e.g. one copy picks up an `env(...)` the other doesn't).

**A concrete sketch for this project**, following that precedent, once `PackageAnnounce` and the new `PackageClaim` both flatten the shared registry-target options struct proposed in Finding 5:

```rust
#[test]
fn announce_and_claim_share_the_registry_target_flags() {
    use clap::CommandFactory;

    let announce = crate::command::package_announce::PackageAnnounce::command();
    let claim = crate::command::package_claim::PackageClaim::command();

    // Every id the shared RegistryTarget options group is responsible for.
    let shared_ids = ["index_repo", "forge", "fork", "out", "transport"];

    for id in shared_ids {
        let a = announce.get_arguments().find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("`package announce` is missing shared arg `{id}`"));
        let c = claim.get_arguments().find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("`package claim` is missing shared arg `{id}`"));

        assert_eq!(a.get_long(), c.get_long(), "`{id}` long spelling diverged");
        assert_eq!(a.get_value_names(), c.get_value_names(), "`{id}` value name diverged");
        assert_eq!(a.is_required_set(), c.is_required_set(), "`{id}` required-ness diverged");
        assert_eq!(a.get_default_values(), c.get_default_values(), "`{id}` default diverged");
    }
}
```

**Honesty check per this repo's own `quality-core.md` "Unchecked Green" rule** — this test is not automatically meaningful just because it exists: if both commands flatten the *literal same struct type* (the Finding 5 recommendation), the `Arg` definitions are byte-identical by construction and this test can only ever pass, which makes it a green that cannot be told from "never ran." To make it a real check with a reachable red, mutate one sibling's flatten call during test-writing (temporarily hand-roll one field outside the shared struct with a different `value_name`) and confirm the assertion fails, then revert — the same "prove the mutation landed" discipline the project already requires of its guards, applied here once during authorship rather than kept as a standing self-test (a standing self-test would need the *code* to occasionally not-flatten, which defeats the point of sharing the struct in the first place). The right permanent value of this test is as a tripwire for a **future refactor** that stops flattening the shared struct for one sibling — at authorship time, prove it catches that by making the mutation, watching it fail, then reverting to the flattened form before committing.

**Existing per-flag parity suite in this repo?** No. `crates/ocx_cli/src/options/lazy_mode.rs` (named in the task as a candidate) has its own `#[cfg(test)] mod tests` with 15 `#[test]` functions, but they test `LazyMode::mode()`'s resolution logic in isolation — none of them call `get_arguments()`, compare two commands, or check cross-command parity. A repo-wide search for `get_arguments`, `ArgGroup`, or `value_source`/`ValueSource` inside `crates/ocx_cli/src/` turns up only `ArgGroup` usage (`package_push.rs`, for the sign/sbom/modifier exclusivity groups quoted in Finding 2) — no `get_arguments()` or `value_source` call anywhere in the crate. This is a genuine gap the plan is right to want closed, and there is no in-repo test to adapt — model the new test on the `foundry-rs` sketch above, not on anything already in this repo.

**Citations:** [`Command::get_arguments`, docs.rs](https://docs.rs/clap/latest/clap/struct.Command.html) (method listed; full doc text is truncated behind docs.rs's rendering in this research and not independently re-verified — see Negative findings) · [`foundry-rs/foundry`, `crates/cli/src/opts/rpc.rs`](https://github.com/foundry-rs/foundry/blob/master/crates/cli/src/opts/rpc.rs) · [`foundry-rs/foundry`, `crates/cli/src/opts/evm.rs`](https://github.com/foundry-rs/foundry/blob/master/crates/cli/src/opts/evm.rs) · in-repo: `crates/ocx_cli/src/options/lazy_mode.rs`, `crates/ocx_cli/src/command/package_push.rs`.

## Recommendation

**(a) Flag→positional window for `ocx package announce --package <ns>/<pkg>` → `ocx package announce <ns>/<pkg>`:**

- Two fields on `PackageAnnounce`, not one: keep the positional field named `package: Option<options::Identifier>`, add a new hidden field (e.g. `package_flag: Option<options::Identifier>`) with `#[clap(long = "package", hide = true, conflicts_with = "package")]`.
- Positional gets `#[clap(required_unless_present = "package_flag")]` (or wrap both in a `required(true)` `ArgGroup` if the combined clap error message is preferred over two independent single-arg rules — this repo already has a working `ArgGroup` precedent in `package_push.rs` to copy from).
- Merge with `self.package_flag.clone().or_else(|| self.package.clone())`; do this once, at the top of `execute()`, never re-derive it.
- Detect with `self.package_flag.is_some()` — no `ArgGroup::id()` lookup, no `value_source`. Call `super::deprecated::warn_renamed(&context, "package announce --package", "package announce <IDENTIFIER>")` from inside `PackageAnnounce::execute`, not from the `Package` enum dispatch (that dispatch site has no visibility into which `Arg` on the child struct fired).
- `REMOVAL_RELEASE` in `deprecated.rs` is a single shared constant for the whole file's window — confirm this rename lands inside the currently-open 0.6→0.7 window (it does, per the task) before adding it, rather than opening a second window.
- Add the regression test `deprecated.rs` itself lacks today (Finding 1's negative finding), modeled on jj's `test_bisect_command.rs` shape: assert the warning text appears in stderr and does not appear in stdout, for both the `--format json` and default format paths (the JSON envelope path is exactly where a stray stdout write would corrupt a machine-readable document — this repo's own `render_error_envelope` code comment already worries about exactly this class of stdout corruption).

**(b) Shared options struct + parity test for `--index-repo`/`--forge`/`--fork`/`--out`/`--transport`:**

- One new module, `crates/ocx_cli/src/options/registry_target.rs` (naming is the plan's call), following the `LazyMode` shape exactly: `#[derive(clap::Args, Clone, Debug, Default)]`, each field documented and attached with its real `#[clap(long = "...")]`, any shared resolution logic (e.g. forge-inference from the index-repo host, or the `--out`/`--fork` mutual exclusion already declared today) implemented as methods on the struct, never re-derived at each call site. Re-export from `options.rs` (`pub use registry_target::RegistryTarget;`) unless a second, independent effort needs to reach it before the first lands (in which case use the `pub mod` escape hatch, per the documented convention).
- Flatten the identical struct type into both `PackageAnnounce` and the new `PackageClaim` — do not let `claim` redeclare any of these five flags independently, even if the two commands' `--out`/`--fork` semantics differ slightly; a semantic difference belongs in the struct's own resolution method (parameterized, or a second narrow field), not in a second copy of the flag declaration.
- Add the `get_arguments()`/`get_id()` parity test sketched in Finding 6, directly modeled on `foundry-rs/foundry`'s `rpc.rs`/`evm.rs` pair. Prove it can go red once, at authorship time, by temporarily un-flattening one sibling's copy of one field with a different `value_name`, confirming the assertion fires, then reverting to the shared-flatten form before committing — per this repo's own "Unchecked Green" rule, a parity test over two commands that share one struct by construction is not a real check until its red path has actually been exercised once.

## Negative findings

- No test exists for `deprecated.rs`'s `warn_renamed` or the hidden-variant dispatch anywhere in `crates/ocx_cli/`; nothing in-repo to model a "prove the warning is stderr-only" test on. Recommendation (a) above proposes filling this gap alongside the new migration, not before it.
- Could not locate a citable rustup source snippet implementing a deprecation-warning code path (only GitHub issues *about* rustup warnings were found) — cannot cite rustup's actual mechanism, only that it exists as user-visible behavior.
- Could not locate a citable uv source snippet that *emits* its deprecated `--legacy-*`-style warning (found only the `hide = true` `Arg` definition in `crates/uv-cli/src/lib.rs`, not the code path that checks it and warns) — cannot confirm uv's warning routing (stdout vs stderr) from source, only infer it follows the same convention as its `hide = true` definition.
- Could not confirm jj's specific *removal* announcement wording for a past flag→positional migration (only the introduction-of-deprecation wording was found, for a currently-still-present deprecated flag) — cannot cite a "here is what jj's changelog said the day it actually deleted an old flag" example.
- `docs.rs`'s rendered HTML for `Command::get_arguments`'s full doc comment and `Arg::conflicts_with`'s full doc comment repeatedly truncated in fetches; the exact prose of those two doc comments is not independently quoted here — the *behavior* (iterate `&Arg`s; conflicts are registered symmetrically from either side) is instead corroborated by (i) real call sites in `foundry-rs/foundry` and other repos for `get_arguments`, and (ii) this project's own `package_announce.rs`, which already relies on one-directional `conflicts_with_all` declarations without the reverse declaration existing — treat the conflicts-symmetry claim as empirically well-supported, not verbatim-cited from clap's own doc text.
- `crates/ocx_cli/src/command/options/` (the path given in the task prompt) does not exist; the real location is `crates/ocx_cli/src/options/` (one level up, a sibling of `command/`, not nested inside it). All findings above use the real path.
- No `package_claim.rs` (or any `*claim*` file) exists yet anywhere under `crates/ocx_cli/src/` — confirmed by directory search. Everything about `PackageClaim`'s shape in this document is therefore this research's own extrapolation from the task's stated design ("already takes a positional," shares the five named flags), consistent with what `.claude/artifacts/adr_index_claim_command.md` and `.claude/artifacts/system_design_index_claim_command.md` (both present in this repo, not read in depth for this axis) appear to already specify — the plan should treat those two documents, not this one, as authoritative on `claim`'s actual argument shape.
