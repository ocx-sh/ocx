# Pre-stub evidence: interpolation token grammar (WP2)

This file records the **current** behaviour of the five code sites that WP2 of
`plan_interpolation_token_grammar` deletes or rewrites, captured against the tree WP2 starts
from. It exists because those behaviours are unreachable once the stub lands: the `str::replace`
step, the two-phase substitution, the `strip_prefix("${installPath}/")` classifier and the
`contains("${installPath}")` guard are all removed by the scanner rewrite, so after the stub the
only way to red these assertions would be against a mutant — much weaker evidence than a red
state against real, shipped code. It is the mandated mitigation for Constitution Deviation 1
(a hand-written parser on a published wire format): each assertion below compares today's answer
against the value WP2 makes correct, so the captured failure output *is* the before/after record.

**Capture conditions.** Worktree `/home/mherwig/dev/ocx/.agents/worktrees/wp2-scanner`, branch
`hex/interpolation-token-grammar--wp2-scanner`, at `92afe4ee` (`feat(metadata): render
interpolation token paths as native or posix` — WP1's `render.rs`, a new module that touches
none of the five behaviours below). For every behaviour here the tree is `main`.

Sections 1–4 were captured by throwaway `#[cfg(test)]` tests written into the files where the
functions live, run once, and reverted — the tree is clean. Section 5 is an existing test, run
unmodified. Every "Captured output" block below is pasted verbatim from `cargo test` stdout.

Command for §1–§4 (all six in one run):

```
cargo test -p ocx_lib --lib prestub_
```

---

## 1. `$${installPath}` double-resolves today — C-001, S-020

### (a) Code under test

`crates/ocx_lib/src/package/metadata/template.rs:176` and `:211` — the `str::contains` /
`str::replace` pair. `"$${installPath}"` contains the byte sequence `"${installPath}"` starting
at index 1, so the guard passes and `replace` rewrites that inner match, leaving the leading `$`:

```rust
let value = if template.contains("${installPath}") {
    …
    template.replace("${installPath}", &install_lossy)
```

### (b) Throwaway assertions

```rust
/// Evidence 1a (C-001, S-020): `$${installPath}` double-resolves today.
#[test]
fn prestub_escaped_install_path_token_resolves_to_dollar_plus_path() {
    let dir = TempDir::new().unwrap();
    let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
    let resolver = TemplateResolver::new(dir.path(), &contexts);

    assert_eq!(
        resolver.resolve("$${installPath}").unwrap(),
        "${installPath}",
        "after D2 the escape must emit the literal token; left is today's answer"
    );
}

/// Evidence 1b (C-001, S-020): `$$${installPath}` today.
#[test]
fn prestub_double_escaped_install_path_token_resolves_to_two_dollars_plus_path() {
    let dir = TempDir::new().unwrap();
    let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
    let resolver = TemplateResolver::new(dir.path(), &contexts);

    assert_eq!(
        resolver.resolve("$$${installPath}").unwrap(),
        "$${installPath}",
        "after D2: literal $ then a fired escape; left is today's answer"
    );
}
```

### (c) Captured output

```
---- package::metadata::template::tests::prestub_escaped_install_path_token_resolves_to_dollar_plus_path stdout ----

thread 'package::metadata::template::tests::prestub_escaped_install_path_token_resolves_to_dollar_plus_path' (1178996) panicked at crates/ocx_lib/src/package/metadata/template.rs:787:9:
assertion `left == right` failed: after D2 the escape must emit the literal token; left is today's answer
  left: "$/tmp/.tmpdDZXCt"
 right: "${installPath}"
```

```
---- package::metadata::template::tests::prestub_double_escaped_install_path_token_resolves_to_two_dollars_plus_path stdout ----

thread 'package::metadata::template::tests::prestub_double_escaped_install_path_token_resolves_to_two_dollars_plus_path' (1178995) panicked at crates/ocx_lib/src/package/metadata/template.rs:801:9:
assertion `left == right` failed: after D2: literal $ then a fired escape; left is today's answer
  left: "$$/tmp/.tmpQDKDmq"
 right: "$${installPath}"
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

`/tmp/.tmpdDZXCt` and `/tmp/.tmpQDKDmq` are the per-test `TempDir` install paths — i.e. today's
answers are literally `$<content-path>` and `$$<content-path>`.

### (d) Contract

**C-001 / S-020.** After WP2 (D2), `$${installPath}` resolves to the literal `${installPath}`
and `$$${installPath}` to the literal `$${installPath}`. This is the ADR's **only** change to
already-published read-path behaviour and the reason WP2 ships as `feat(metadata)!:`
(Constitution Deviation 5).

---

## 2. The injected-bytes case — C-009

### (a) Code under test

`crates/ocx_lib/src/package/metadata/template.rs:200-210` — the install-path `${` injection
defence, which exists precisely because substitution is two-phase (`str::replace` writes the
install path in at `:211`, then `DEP_TOKEN_PATTERN.captures_iter` re-reads those bytes at `:228`):

```rust
if let Some(idx) = install_lossy.find("${") {
    …
    return Err(TemplateError::UnknownPlaceholder { placeholder });
}
```

### (b) Throwaway assertions

Leg (a) is the case the plan names. Leg (b) is its counterfactual: the *same byte sequence*
arriving as publisher text, which shows what phase 2 does to those bytes when the defence is not
in the way — that is the re-read hazard D1 removes structurally.

```rust
/// Evidence 2a (C-009): an install path whose bytes spell a dep token.
#[test]
fn prestub_install_path_bearing_dep_token_bytes_survives_verbatim() {
    use std::path::PathBuf;

    let dep_dir = TempDir::new().unwrap();
    let injected = PathBuf::from("/opt/${deps.foo.installPath}/x");
    let mut contexts = HashMap::new();
    contexts.insert(dep_name("foo"), ctx(dep_dir.path(), "foo"));
    let resolver = TemplateResolver::new(&injected, &contexts);

    let outcome = match resolver.resolve("${installPath}/tool") {
        Ok(value) => value,
        Err(error) => format!("<Err: {error}>"),
    };
    assert_eq!(
        outcome, "/opt/${deps.foo.installPath}/x/tool",
        "C-009 requires the injected bytes verbatim; left is today's answer"
    );
}

/// Evidence 2b (C-009): what phase 2 does to those same bytes when they
/// arrive as publisher text — the re-read the defence pre-empts.
#[test]
fn prestub_dep_token_bytes_as_publisher_text_are_substituted_by_phase_two() {
    let self_dir = TempDir::new().unwrap();
    let dep_dir = TempDir::new().unwrap();
    let mut contexts = HashMap::new();
    contexts.insert(dep_name("foo"), ctx(dep_dir.path(), "foo"));
    let resolver = TemplateResolver::new(self_dir.path(), &contexts);

    let outcome = match resolver.resolve("/opt/${deps.foo.installPath}/x/tool") {
        Ok(value) => value,
        Err(error) => format!("<Err: {error}>"),
    };
    assert_eq!(
        outcome, "/opt/${deps.foo.installPath}/x/tool",
        "the same byte sequence is rewritten by the dep regex; left is today's answer"
    );
}
```

### (c) Captured output

```
---- package::metadata::template::tests::prestub_install_path_bearing_dep_token_bytes_survives_verbatim stdout ----

thread 'package::metadata::template::tests::prestub_install_path_bearing_dep_token_bytes_survives_verbatim' (1178997) panicked at crates/ocx_lib/src/package/metadata/template.rs:823:9:
assertion `left == right` failed: C-009 requires the injected bytes verbatim; left is today's answer
  left: "<Err: contains unknown placeholder '${deps.foo.installPath}'>"
 right: "/opt/${deps.foo.installPath}/x/tool"
```

```
---- package::metadata::template::tests::prestub_dep_token_bytes_as_publisher_text_are_substituted_by_phase_two stdout ----

thread 'package::metadata::template::tests::prestub_dep_token_bytes_as_publisher_text_are_substituted_by_phase_two' (1178994) panicked at crates/ocx_lib/src/package/metadata/template.rs:843:9:
assertion `left == right` failed: the same byte sequence is rewritten by the dep regex; left is today's answer
  left: "/opt//tmp/.tmp7hd7vk/x/tool"
 right: "/opt/${deps.foo.installPath}/x/tool"
```

**Reading it.** The defence at `:200-208` **does fire** — today's answer for the plan's case is
not a corrupted string but `UnknownPlaceholder`, exit 65, on a legal install path. Leg (b) shows
why the defence had to exist: fed the identical byte sequence, phase 2's regex substitutes
`foo`'s install path (`/opt//tmp/.tmp7hd7vk/x/tool`), so without the guard, filesystem bytes
would be re-interpreted as a publisher token.

### (d) Contract

**C-009.** After WP2, the single-pass scanner never re-examines output bytes, so the install
path's `${deps.foo.installPath}` bytes are emitted verbatim and the expected value becomes
`/opt/${deps.foo.installPath}/x/tool` — an `Ok`, not an error. D12 accordingly **deletes** the
injection defence as structurally unnecessary; this evidence is what makes that deletion a
behaviour improvement (65 → success) rather than a removed guard.

---

## 3. `classify_install_path_rooted_dir` misses the alias — C-010

### (a) Code under test

`crates/ocx_lib/src/package/metadata/template.rs:88-90`:

```rust
pub fn classify_install_path_rooted_dir(value: &str) -> Option<RelativePath> {
    const INSTALL_PATH_DIR_PREFIX: &str = "${installPath}/";
    let rel = value.strip_prefix(INSTALL_PATH_DIR_PREFIX)?;
```

`${installPath}/` is not a prefix of `${self.installPath}/bin`, so the `?` returns `None`.

### (b) Throwaway assertion

Both spellings are asserted in one tuple so the failure output carries the contrast on one line —
a lone alias assertion would not show that the bare form works.

```rust
/// Evidence 3 (C-010): the alias is a silent `None` today.
#[test]
fn prestub_classify_install_path_rooted_dir_misses_the_self_alias() {
    use std::path::PathBuf;

    let bare = classify_install_path_rooted_dir("${installPath}/bin").map(|rel| rel.as_path().to_path_buf());
    let alias = classify_install_path_rooted_dir("${self.installPath}/bin").map(|rel| rel.as_path().to_path_buf());

    assert_eq!(
        (bare, alias),
        (Some(PathBuf::from("bin")), Some(PathBuf::from("bin"))),
        "after D10 both spellings classify to bin; left is (bare, alias) today"
    );
}
```

### (c) Captured output

```
---- package::metadata::template::tests::prestub_classify_install_path_rooted_dir_misses_the_self_alias stdout ----

thread 'package::metadata::template::tests::prestub_classify_install_path_rooted_dir_misses_the_self_alias' (1178993) panicked at crates/ocx_lib/src/package/metadata/template.rs:857:9:
assertion `left == right` failed: after D10 both spellings classify to bin; left is (bare, alias) today
  left: (Some("bin"), None)
 right: (Some("bin"), Some("bin"))
```

### (d) Contract

**C-010.** `${installPath}/bin` → `Some("bin")` today (unchanged); `${self.installPath}/bin` →
`None` today, and after WP2 (D10) → `Some("bin")`. A silent wrong answer: the var is dropped from
`bin_scan`'s scan scope, so the binaries claim comes out short with no diagnostic.

---

## 4. `libc_lint` fails open for the alias — C-011

### (a) Code under test

`crates/ocx_lib/src/package/libc_lint.rs:236` inside `resolve_scan_scope` (`:214`):

```rust
for segment in path_var.value.split(':') {
    if !segment.contains(INSTALL_PATH_TOKEN) {
        continue;
    }
```

`INSTALL_PATH_TOKEN` is `"${installPath}"` (`:219`), which is not a substring of
`${self.installPath}`, so every segment is `continue`d — the segment does not reach the
`classify_install_path_rooted_dir` arm at `:239` and is therefore never pushed to
`unresolvable` either.

### (b) Throwaway assertion

```rust
/// Evidence 4 (C-011): the `contains("${installPath}")` guard skips every
/// segment for the `self.` spelling, so the scan scope comes out empty and
/// the segment is not even recorded in `unresolvable` — the lint reports
/// "nothing to check" on a package it has not checked.
#[test]
fn prestub_resolve_scan_scope_is_empty_for_the_self_alias() {
    fn scope_of(value: &str) -> (Vec<PathBuf>, Vec<String>) {
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"env":[{{"key":"PATH","type":"path","value":"{value}","required":false,"visibility":"interface"}}]}}"#
        ))
        .expect("fixture metadata parses");
        let scope = resolve_scan_scope(&metadata);
        (
            scope
                .directories
                .iter()
                .map(|relative| relative.as_path().to_path_buf())
                .collect(),
            scope.unresolvable,
        )
    }

    let bare = scope_of("${installPath}/bin");
    let alias = scope_of("${self.installPath}/bin");

    assert_eq!(
        (bare, alias),
        (
            (vec![PathBuf::from("bin")], Vec::new()),
            (vec![PathBuf::from("bin")], Vec::new())
        ),
        "after D10 both spellings yield the same scan scope; \
         left is ((bare dirs, bare unresolvable), (alias dirs, alias unresolvable)) today"
    );
}
```

The two documents are identical apart from the token spelling.

### (c) Captured output

```
---- package::libc_lint::tests::prestub_resolve_scan_scope_is_empty_for_the_self_alias stdout ----

thread 'package::libc_lint::tests::prestub_resolve_scan_scope_is_empty_for_the_self_alias' (1178992) panicked at crates/ocx_lib/src/package/libc_lint.rs:633:9:
assertion `left == right` failed: after D10 both spellings yield the same scan scope; left is ((bare dirs, bare unresolvable), (alias dirs, alias unresolvable)) today
  left: ((["bin"], []), ([], []))
 right: ((["bin"], []), (["bin"], []))
```

**Reading it.** `${installPath}/bin` yields `directories = ["bin"]`, `unresolvable = []`.
The alias yields `directories = []`, `unresolvable = []` — an **empty** scan scope with nothing
recorded as unresolvable. Per the doc comment at `libc_lint.rs:190-191`, "Empty means the package
puts nothing of its own on `PATH` — nothing to check", so the lint passes vacuously: a
glibc/musl mismatch would ship unnoticed on an otherwise fail-closed lint.

### (d) Contract

**C-011.** After WP2 (D10), `resolve_scan_scope` routes each segment through the scanner and the
alias yields the same non-empty scope as the bare spelling — `directories = ["bin"]`. Note that
this hazard is **not** a regression from today: `${self.installPath}` is currently unpublishable,
so D4 creates the hazard and D10 must close it in the same release (the plan records this as a
review checkpoint on WP2's diff, not a merge-order rule).

---

## 5. D14's sharpest red state is real code — the inspect test inverts

### (a) Code under test

`crates/ocx_lib/src/package_manager/tasks/inspect.rs:1533`, in `mod spec_tests`
(declared at `:1149` — the plan cites this test as `inspect::tests::…`; its actual module path
is `package_manager::tasks::inspect::spec_tests`). It asserts that `inspect` in default mode,
over a document whose env value references an undeclared dependency, surfaces
`PackageErrorKind::Internal`, because `ValidMetadata::try_from` rejects the document at the
ingress boundary:

```rust
const BAD_METADATA_JSON: &str = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","type":"constant","value":"${deps.missing.installPath}/x","visibility":"public"}],"entrypoints":{}}"#;
…
assert!(
    matches!(err, PackageErrorKind::Internal(_)),
    "malformed metadata must surface Internal (→ DataError/65), got {err:?}"
);
```

### (b) Command run

Not modified — the existing test, run as-is:

```
cargo test -p ocx_lib --lib inspect_default_malformed_metadata_is_internal
```

### (c) Captured output

```
running 1 test
test package_manager::tasks::inspect::spec_tests::inspect_default_malformed_metadata_is_internal ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3609 filtered out; finished in 0.01s
```

### (d) Contract

**D14** (plan: "Executable phases → Pre-stub evidence capture"; ADR D14). Refusal is scoped to
*resolution*, not to *reading*: token validation leaves `ValidMetadata::try_from` and becomes an
explicit publish gate plus the resolver's own failure. `inspect` is on the read-only side of the
D14 table, so after WP2 this exact document must make `inspect` **succeed** and show
`${deps.missing.installPath}/x` verbatim, instead of erroring with `PackageErrorKind::Internal`.
The assertion above therefore inverts, and its green state today is the proof that WP2's change
is a real behaviour change on live code rather than a no-op. The test was not modified.

---

## Coverage and gaps

| # | Behaviour | Contract | Captured |
|---|---|---|---|
| 1 | `$${installPath}` / `$$${installPath}` double-resolve | C-001, S-020 | red, verbatim |
| 2 | injected install-path bytes; defence fires; phase-2 re-read counterfactual | C-009 | red, verbatim |
| 3 | `classify_install_path_rooted_dir` alias → `None` | C-010 | red, verbatim |
| 4 | `libc_lint` alias → empty scan scope, nothing in `unresolvable` | C-011 | red, verbatim |
| 5 | `inspect` on an undeclared-dep token → `Internal` | D14 | green, verbatim |

Nothing in the assigned set was unreachable from a unit test; all five were captured as
specified. Two notes for WP2's reviewers:

- **§2's plan wording ("goes red under a two-phase substitution") is satisfied by an error, not a
  corrupted string.** The defence at `template.rs:200-208` pre-empts the corruption for the exact
  case named, so the captured left-hand side is `UnknownPlaceholder`. Leg (b) supplies the
  corruption itself on the same byte sequence. Both are needed: the first records what a publisher
  sees today (exit 65 on a legal install path), the second records what the guard is guarding.
- **§4's throwaway test calls the private `resolve_scan_scope` directly** rather than driving
  `check_declared_libc` end-to-end. The end-to-end leg is available (the module's existing tests
  build a real content tree with `glibc_binary`) but would assert only "no error", which is the
  same answer a correctly-scoped, correctly-declared package gives — indistinguishable from a
  check that never ran. Asserting on the scope value is the discriminating form.
