// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Interpolation-token resolution for metadata string fields.
//!
//! OCX claims every `${…}` sequence. Four bodies are recognised —
//! `${installPath}`, its exact alias `${self.installPath}`, `${self.env.KEY}`
//! and `${deps.NAME.installPath}` — each optionally carrying a `:native` or
//! `:posix` render modifier. Anything else is a hard error; `$${` is the only
//! way to emit a literal `${…}`. Recognition lives in one place,
//! [`scanner::scan`]; this module gates the classified tokens against the
//! active [`AllowedTokens`] set, substitutes them, and renders the result.
//!
//! Consumed by `env::resolver::EnvResolver`, the entrypoint arg resolver, and
//! the publish gate in `validation`.
//!
//! One concern per file. This one is the resolver: it drives the other five
//! and owns nothing they own.
//!
//! | File | Concern |
//! |---|---|
//! | `scanner.rs` | Recognition — what a `${…}` **is** |
//! | `gate.rs` | Capability — which token classes a surface may carry |
//! | `scope.rs` | The `${self.env.KEY}` lookup scope |
//! | `error.rs` | The refusal taxonomy and how each refusal reads |
//! | `render.rs` | Path-separator rendering |
//!
//! `gate`, `scope` and `error` are private modules whose public items are
//! re-exported here, so the crate sees one path per item — `template::Usage`,
//! not `template::gate::Usage`.

mod error;
mod gate;
pub mod render;
pub mod scanner;
mod scope;

pub use error::{TemplateError, UnknownTokenHint};
pub use gate::{AllowedTokens, Usage, first_disallowed_token};
pub use scope::{DeclaredVar, SelfEnvScope};

use std::collections::HashMap;
use std::path::Path;

use self::render::{Host, RenderModifier, render};
use self::scanner::{Segment, Token, TokenShape};
use super::env::dep_context::DependencyContext;
use super::env::entry::Entry;
use crate::package::metadata::dependency::DependencyName;
use crate::utility::fs::path::RelativePath;

/// The byte ceiling on one resolved template value.
///
/// `${self.env.KEY}` substitutes the referenced var's already-**resolved**
/// value, so a var that names the previous one twice doubles per var while the
/// authored metadata stays flat: 41 vars — under 4 KiB serialized, and accepted
/// by `validate_for_publish`, which never resolves — expand to 32 MiB at
/// compose time. That runs on every `ocx env` / `ocx run` / `ocx package exec` /
/// launcher re-entry, and is reachable through a **transitive dependency's**
/// metadata, so the victim never named the document that did it
/// (CWE-400/409/776).
///
/// A per-value cap is what closes it: the chain manifests as one var exceeding
/// the cap, so resolution stops at the first offender instead of after the
/// document. A count cap on `Env.variables` would additionally refuse documents
/// already published, and buys nothing here (256 vars still permit 2²⁵⁵).
///
/// An **interface number**: a value it refuses is a value that stops resolving,
/// so the owner may move it. Nothing derives from it.
pub const MAX_RESOLVED_VALUE_BYTES: usize = 64 * 1024;

/// Classifies a `Path`-modifier env var's raw value template as an
/// install-path-rooted `PATH` directory, extracting the relative directory
/// under the install root.
///
/// Returns `Some(rel)` only when the scan of `value` is exactly one
/// modifier-free [`scanner::TokenShape::InstallPath`] token followed by one
/// `/…` literal — so both
/// `${installPath}/bin` and `${self.installPath}/bin` classify to `bin`
/// (D4/D10). Any other shape returns `None`: a bare `${installPath}`, a
/// combined-path var, a literal path, or a modifier-bearing token
/// (`${self.installPath:posix}/bin`) — best-effort scan-scope exclusion, not
/// an error. A scan **error** returns `None` for the same reason, and under
/// D14 that is a reachable case rather than a defensive one: this runs on read
/// paths, which no longer refuse an unrecognised token. `rel` is parsed through
/// [`RelativePath::parse`] so a malformed or escaping `<rel>` is excluded the
/// same way.
///
/// Consumed by `package::bin_scan::scan_interface_binaries` (create-time
/// executable auto-scan) to identify which declared `Path` vars are scan
/// targets. Does not itself check `modifier`/`visibility` — those live on
/// the `Var` struct, not the raw value string, and are the caller's
/// responsibility. See `adr_declared_binaries_metadata.md` §2 steps 1-2.
#[must_use]
pub fn classify_install_path_rooted_dir(value: &str) -> Option<RelativePath> {
    let segments = scanner::scan(value).ok()?;
    let [Segment::Token(token), Segment::Literal { text: rest, .. }] = segments.as_slice() else {
        return None;
    };
    if token.shape != TokenShape::InstallPath || token.modifier.is_some() {
        return None;
    }
    RelativePath::parse(rest.strip_prefix('/')?).ok()
}

/// Resolves recognised interpolation tokens in template strings.
///
/// Build once with [`TemplateResolver::new`], call [`TemplateResolver::resolve`] for each
/// template string. Callers that iterate over many variables (e.g. `Exporter`) benefit from
/// building the resolver once and reusing it.
///
/// [`TemplateResolver::resolve`] substitutes real install paths and verifies each dep's
/// `install_path.exists()` on disk. [`TemplateResolver::resolve_without_existence_checks`]
/// is the same substitution with every filesystem assertion suppressed, for callers that
/// must resolve a value they are not going to emit (D8).
///
/// Use [`TemplateResolver::usage`] to restrict which token classes are allowed (e.g.,
/// `Usage::EntryPointArgs` forbids `${deps.*}` and `${self.env.*}` tokens), and
/// [`TemplateResolver::with_self_env`] to supply the scope `${self.env.KEY}` resolves
/// against. The resolver stays **pure** (D5): every input is a parameter, and nothing
/// here reads the process environment.
pub struct TemplateResolver<'a> {
    install_path: &'a Path,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
    allowed: AllowedTokens,
    host: Host,
    self_env: &'a SelfEnvScope<Entry>,
}

impl<'a> TemplateResolver<'a> {
    pub fn new(install_path: &'a Path, dep_contexts: &'a HashMap<DependencyName, DependencyContext>) -> Self {
        Self {
            install_path,
            dep_contexts,
            // Default: Environment caps — every recognised token permitted.
            // Preserves today's behavior for every existing caller that does not call .usage().
            allowed: Usage::Environment.into(),
            host: Host::current(),
            // Empty by default: a caller that supplies no env context has no
            // earlier-declared var for `${self.env.KEY}` to name, which is the
            // undefined case rather than a special one.
            self_env: &scope::EMPTY_SELF_ENV,
        }
    }

    /// Sets the interpolation usage, restricting which token classes are permitted.
    ///
    /// Pass a [`Usage`] variant or an [`AllowedTokens`] value directly. The default
    /// is [`Usage::Environment`] (all tokens permitted), matching the existing behavior
    /// for env-value interpolation callers.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resolver = TemplateResolver::new(path, &deps).usage(Usage::EntryPointArgs);
    /// ```
    #[must_use]
    pub fn usage(mut self, usage: impl Into<AllowedTokens>) -> Self {
        self.allowed = usage.into();
        self
    }

    /// Supplies the scope `${self.env.KEY}` resolves against: the entries this
    /// package's env vars **declared strictly earlier** already resolved to, in
    /// declaration order (D6.1).
    ///
    /// A parameter rather than an ambient read, because D5 requires the resolver
    /// to stay pure — and a growing scope cannot be a long-lived borrow: the
    /// composer pushes into its private accumulator between vars, so the scope
    /// is handed in per resolve call.
    ///
    /// Omitting it leaves the scope empty, which is what every caller outside
    /// env-value resolution wants: entrypoint args carry no self-env scope, and
    /// `Usage::EntryPointArgs` refuses the token outright.
    #[must_use]
    pub fn with_self_env(mut self, self_env: &'a SelfEnvScope<Entry>) -> Self {
        self.self_env = self_env;
        self
    }

    /// Pins the host a `:posix` modifier renders for, so both legs of the
    /// modifier contracts run on any CI host.
    ///
    /// Test-only seam per `arch-principles.md`: it covers [`render`]'s host
    /// argument alone and must never divert `dunce::simplified`, which stays a
    /// real `cfg(windows)` call.
    #[cfg(any(test, feature = "__testing"))]
    #[must_use]
    pub fn with_host(mut self, host: Host) -> Self {
        self.host = host;
        self
    }

    /// Resolves every recognised token in `template`.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError`] if `template` carries a `${…}` OCX does not
    /// recognise, a token the active [`AllowedTokens`] set forbids, a
    /// `${deps.*}` token naming an unknown dependency or field, or a
    /// dependency that is not installed on disk.
    pub fn resolve(&self, template: &str) -> Result<String, TemplateError> {
        self.resolve_inner(template, /* check_exists = */ true)
    }

    /// Resolves every recognised token in `template` **without** asserting that
    /// a referenced dependency exists on disk.
    ///
    /// The composer's split (D8) is *value resolution always; every filesystem
    /// assertion on emit only*, so a var that never crosses the active surface
    /// resolves through here: a declared-but-uninstalled dep must not turn a
    /// working install into exit 79 on a value nobody emits.
    ///
    /// # Errors
    ///
    /// As [`TemplateResolver::resolve`], minus
    /// [`TemplateError::DependencyNotInstalled`].
    pub fn resolve_without_existence_checks(&self, template: &str) -> Result<String, TemplateError> {
        self.resolve_inner(template, /* check_exists = */ false)
    }

    /// Scan, then gate, then substitute, then render — in that order.
    ///
    /// The gate runs over the whole scan, not per segment, so a disallowed
    /// token anywhere in `template` is refused before any `dep_contexts` lookup
    /// happens — and it is the same [`first_disallowed_token`] the publish gate
    /// calls. Substituted bytes go straight to the output buffer and are never
    /// re-examined, which is what makes an install path containing `${…}` inert
    /// (C-009).
    ///
    /// The output buffer is measured against [`MAX_RESOLVED_VALUE_BYTES`] after
    /// every segment. Here rather than at any of the six commands that resolve:
    /// one budget on the one place bytes are produced closes every sink, where
    /// per-command guards would be six guards for one root cause.
    fn resolve_inner(&self, template: &str, check_exists: bool) -> Result<String, TemplateError> {
        let segments = scanner::scan(template)?;
        if let Some(token) = first_disallowed_token(&segments, self.allowed) {
            return Err(TemplateError::DisallowedToken {
                token: token.to_owned(),
            });
        }

        let mut resolved = String::with_capacity(template.len());
        for segment in segments {
            match segment {
                Segment::Literal { text, .. } => resolved.push_str(text),
                Segment::Token(token) => {
                    let value = substitute(
                        &token,
                        self.install_path,
                        self.dep_contexts,
                        self.self_env,
                        check_exists,
                    )?;
                    resolved.push_str(&render(
                        &value,
                        token.modifier.unwrap_or(RenderModifier::Native),
                        self.host,
                    ));
                }
            }

            // After every segment, not once at the end: the growth this bounds
            // arrives one substitution at a time, so a value already over the
            // budget must not be given another token's worth to double.
            if resolved.len() > MAX_RESOLVED_VALUE_BYTES {
                return Err(TemplateError::ResolvedValueTooLarge {
                    limit: MAX_RESOLVED_VALUE_BYTES,
                });
            }
        }
        Ok(resolved)
    }
}

/// Produces one already-gated token's resolved value — before rendering, which
/// the caller applies.
///
/// A free function rather than a method so the inputs it needs are its
/// signature: it sees the install path and the dep contexts, and nothing else
/// of the resolver. It takes no [`AllowedTokens`]: `resolve_inner` gates the
/// whole scan through [`first_disallowed_token`] before the first substitution,
/// so a token reaching here is already permitted.
///
/// `self_env` is the package's own earlier-declared, already-resolved entries
/// — the scope `${self.env.KEY}` names (D6.1). What it substitutes is the
/// referenced var's **resolved value, never its template** (C-029):
/// `${self.env.A}` where `A = "${installPath}/bin"` yields the expanded path, so
/// a token can never be re-read out of a substituted value and the single-pass
/// guarantee (C-009) holds through self-reference too.
///
/// # Errors
///
/// [`TemplateError::UndefinedSelfEnvRef`] / [`TemplateError::AmbiguousSelfEnvRef`]
/// from the `${self.env.*}` lookup, plus the dependency-resolution errors the
/// `Dep` arm raises.
fn substitute(
    token: &Token<'_>,
    install_path: &Path,
    dep_contexts: &HashMap<DependencyName, DependencyContext>,
    self_env: &SelfEnvScope<Entry>,
    check_exists: bool,
) -> Result<String, TemplateError> {
    match &token.shape {
        TokenShape::InstallPath => Ok(content_directory_string(install_path)),
        TokenShape::Dep { name } => {
            let context = dep_contexts
                .get(name)
                .ok_or_else(|| TemplateError::UnknownDependencyRef {
                    ref_name: name.clone(),
                    declared: declared_names(dep_contexts),
                })?;

            let content_directory = context.install_path();
            // Sync `.exists()` is intentional: this is the synchronous
            // resolution API both `EnvResolver` and the entrypoint resolver
            // call, and the probe is a single `stat(2)` against a path the
            // caller is about to open.
            if check_exists && !content_directory.exists() {
                return Err(TemplateError::DependencyNotInstalled {
                    ref_name: name.clone(),
                    dep_identifier: Box::new(context.identifier().clone()),
                });
            }

            Ok(content_directory_string(&content_directory))
        }
        TokenShape::SelfEnv { key } => lookup_self_env(self_env, key),
    }
}

/// The resolved value of the var `key` names, among the ones this package
/// declared **strictly earlier**.
///
/// `scope` is that prefix, in declaration order — so acyclicity is structural
/// rather than checked (D6.3): a forward reference and a self reference are both
/// simply absent from it, and there is no back-edge to detect.
///
/// # Errors
///
/// As [`SelfEnvScope::lookup`].
fn lookup_self_env(scope: &SelfEnvScope<Entry>, key: &str) -> Result<String, TemplateError> {
    scope.lookup(key).map(|entry| entry.value.clone())
}

/// Renders a resolved `content/` directory as the bytes a token substitutes to.
///
/// `dunce::simplified` strips a Windows `\\?\` verbatim prefix and is a no-op
/// on every other path, so this is the identity off Windows. It runs here,
/// below the render modifier, because rendering composes **after** it: no
/// modifier may emit a verbatim prefix (C-016), and a `${installPath}/bin`
/// template joined onto a verbatim path would otherwise produce a
/// mixed-separator string Windows reads as one literal filename.
fn content_directory_string(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().into_owned()
}

/// The dependency names an unresolved `${deps.*}` reference could have named,
/// sorted so the message is reproducible rather than hash-order noise.
fn declared_names(dep_contexts: &HashMap<DependencyName, DependencyContext>) -> Vec<DependencyName> {
    let mut declared: Vec<DependencyName> = dep_contexts.keys().cloned().collect();
    declared.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    declared
}

#[cfg(test)]
mod tests {
    use super::scope::MAX_LISTED_DECLARED_KEYS;
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};
    use crate::oci;
    use crate::package::metadata::dependency::DependencyName;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> oci::PinnedIdentifier {
        let hex = "a".repeat(64);
        let id: oci::Identifier = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    fn ctx(path: &Path, repo: &str) -> DependencyContext {
        DependencyContext::path_only(pinned(repo), path.to_path_buf())
    }

    fn dep_name(s: &str) -> DependencyName {
        DependencyName::try_from(s).unwrap()
    }

    // 1. Literal strings (including empty) pass through unchanged.
    #[test]
    fn literal_passes_through() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        assert_eq!(resolver.resolve("plain text").unwrap(), "plain text");
        assert_eq!(resolver.resolve("").unwrap(), "");
    }

    // 2. ${installPath} is substituted with the resolver's install path.
    //    When it appears twice, both occurrences are replaced.
    #[test]
    fn install_path_substitution() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let install = dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${installPath}/bin").unwrap(),
            format!("{install}/bin"),
        );
        assert_eq!(
            resolver.resolve("${installPath}/bin:${installPath}/lib").unwrap(),
            format!("{install}/bin:{install}/lib"),
        );
    }

    // 3. ${deps.NAME.installPath} is substituted with the named dep's install path.
    #[test]
    fn dep_install_path_substitution() {
        let dep_dir = TempDir::new().unwrap();
        let self_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let dep_path = dep_dir.path().to_string_lossy();
        assert_eq!(resolver.resolve("${deps.dep1.installPath}").unwrap(), dep_path.as_ref(),);
    }

    // 4. Mixed ${installPath} and ${deps.NAME.installPath} in one template are both resolved.
    #[test]
    fn mixed_install_and_dep_tokens() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let self_path = self_dir.path().to_string_lossy();
        let dep_path = dep_dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${installPath}:${deps.dep1.installPath}/bin").unwrap(),
            format!("{self_path}:{dep_path}/bin"),
        );
    }

    /// S2-prime: positional fidelity for mixed `${installPath}` and
    /// `${deps.NAME.installPath}` resolution. Pins that each substitution
    /// lands at the exact position where the corresponding token appeared
    /// in the template — not anywhere else, no token swap, no duplicated
    /// substitution. The template uses a `:` separator (PATH-style) and a
    /// `/share` suffix on the dep arm so a transposition would be visible
    /// in the assertion.
    #[test]
    fn mixed_install_path_and_deps_install_path_resolve_correctly() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("a"), ctx(dep_dir.path(), "a"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let self_path = self_dir.path().to_string_lossy();
        let dep_path = dep_dir.path().to_string_lossy();

        // PATH-shaped: <install>/bin:<dep_a>/share
        let template = "${installPath}/bin:${deps.a.installPath}/share";
        let resolved = resolver.resolve(template).unwrap();
        let expected = format!("{self_path}/bin:{dep_path}/share");
        assert_eq!(
            resolved, expected,
            "mixed ${{installPath}} + ${{deps.a.installPath}} must land at correct positions; \
             template={template:?} resolved={resolved:?} expected={expected:?}"
        );

        // Sanity: each substituted prefix appears exactly once in the output.
        assert_eq!(
            resolved.matches(self_path.as_ref()).count(),
            1,
            "${{installPath}} must be substituted exactly once: {resolved:?}"
        );
        assert_eq!(
            resolved.matches(dep_path.as_ref()).count(),
            1,
            "${{deps.a.installPath}} must be substituted exactly once: {resolved:?}"
        );
        // No leftover token markers.
        assert!(
            !resolved.contains("${"),
            "no template token markers may remain in resolved output: {resolved:?}"
        );
    }

    // 5. An unknown dep name returns UnknownDependencyRef with the missing name and empty declared list.
    #[test]
    fn unknown_dep_returns_error() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.missing.installPath}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownDependencyRef { ref_name, declared }
                if ref_name.as_str() == "missing" && declared.is_empty()),
            "unexpected error: {err}"
        );
    }

    // 6. A known dep name with an unsupported field returns UnknownField, whose
    //    namespace names the dependency the leaf sits under (D12 — one variant
    //    serves `${self.foo}` and `${deps.x.version}` alike).
    #[test]
    fn unsupported_field_returns_error() {
        let dep_dir = TempDir::new().unwrap();
        let self_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let err = resolver.resolve("${deps.dep1.version}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownField { namespace, field, .. }
                if namespace == "deps.dep1" && field == "version"),
            "unexpected error: {err}"
        );
    }

    // 7. A dep that is in dep_contexts but whose install_path does not exist on disk
    //    returns DependencyNotInstalled.
    #[test]
    fn dep_not_installed_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing_path = dir.path().join("not-there");
        let mut contexts = HashMap::new();
        contexts.insert(
            dep_name("dep1"),
            DependencyContext::path_only(pinned("dep1"), missing_path),
        );
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.dep1.installPath}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DependencyNotInstalled { ref_name, .. }
                if ref_name.as_str() == "dep1"),
            "unexpected error: {err}"
        );
    }

    // 9. C-009 — an install path whose own bytes spell a dep token is emitted
    //    verbatim. INVERTED from the pre-WP2 assertion, which expected
    //    `UnknownPlaceholder`: substitution was two-phase, so the injection
    //    defence at the old `template.rs:200-208` had to pre-empt a re-read.
    //    Single-pass scanning makes the re-read structurally impossible, so the
    //    defence is deleted and the same input is now a success (65 → Ok).
    #[test]
    fn install_path_containing_token_fragment_is_emitted_verbatim() {
        use std::path::PathBuf;

        let dep_dir = TempDir::new().unwrap();
        let injected = PathBuf::from("/opt/${deps.foo.installPath}/x");
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("foo"), ctx(dep_dir.path(), "foo"));
        let resolver = TemplateResolver::new(&injected, &contexts);

        assert_eq!(
            resolver.resolve("${installPath}/tool").unwrap(),
            "/opt/${deps.foo.installPath}/x/tool",
            "substituted bytes must never be rescanned"
        );
    }

    // 8. An uppercase dep name fails the slug class, so `${deps.Python.installPath}`
    //    is not one of the four recognised bodies. INVERTED: it used to pass
    //    through as literal text (the old regex simply did not match); under the
    //    claim-all rule it is a hard error.
    #[test]
    fn uppercase_dep_name_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("python"), ctx(dir.path(), "python"));
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.Python.installPath}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownToken { token, .. }
                if token == "${deps.Python.installPath}"),
            "unexpected error: {err}"
        );
    }

    // ── Contract 10: the resolver under a restricted capability set ───────────
    //
    // The `From<Usage>` mapping itself is pinned beside the gate, in `gate.rs`.

    // Contract 10: resolver with Usage::EntryPointArgs resolves ${installPath} to the
    // install path — the only token class allowed in entrypoint args.
    #[test]
    fn entrypoint_args_usage_resolves_install_path() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();
        let result = resolver.resolve("${installPath}/app.py").unwrap();
        assert_eq!(
            result,
            format!("{install}/app.py"),
            "Usage::EntryPointArgs must resolve ${{installPath}} to the install path"
        );
    }

    // Contract 10c: a bare "${installPath}" (no prefix or suffix) resolves to exactly
    // the install path string under Usage::EntryPointArgs. Pins that the resolver
    // performs a full string replacement, not just a prefix substitution.
    #[test]
    fn entrypoint_args_bare_install_path_resolves_to_exact_path() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();
        let result = resolver.resolve("${installPath}").unwrap();
        assert_eq!(
            result,
            install.as_ref(),
            "bare '${{installPath}}' must resolve to exactly the install path; got {result:?}"
        );
    }

    // C-031 — gate-before-substitution correctness proof (D9).
    //
    // A resolver built with *real, on-disk* dep_contexts and Usage::EntryPointArgs must
    // reject a ${deps.*} template with DisallowedToken BEFORE any substitution.
    // The dep is declared AND its path exists on disk — the gate must fire regardless,
    // proving that the capability gate (not an empty dep_contexts map) is the safety
    // mechanism. A real dep_contexts must never substitute a dep path into an
    // EntryPointArgs template.
    #[test]
    fn entrypoint_args_usage_rejects_deps_token_before_substitution() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap(); // dep path EXISTS on disk

        let mut contexts = HashMap::new();
        // Non-empty dep_contexts with a real, on-disk dep — substitution would
        // resolve this successfully if the capability gate were absent.
        contexts.insert(dep_name("uv"), ctx(dep_dir.path(), "uv"));

        let resolver = TemplateResolver::new(self_dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let err = resolver.resolve("${deps.uv.installPath}/x").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DisallowedToken { token } if token.contains("deps.uv")),
            "expected DisallowedToken with token containing 'deps.uv'; \
             rejected even with a real, on-disk dep context (proves the gate fires before \
             substitution); got: {err}"
        );
    }

    // Contract 10: Usage::Environment still resolves ${deps.*} tokens correctly —
    // the env interpolation path is unaffected by the new capability model.
    #[test]
    fn environment_usage_still_resolves_deps() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();

        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));

        let resolver = TemplateResolver::new(self_dir.path(), &contexts).usage(Usage::Environment);

        let dep_path = dep_dir.path().to_string_lossy();
        let result = resolver.resolve("${deps.dep1.installPath}").unwrap();
        assert_eq!(
            result,
            dep_path.as_ref(),
            "Usage::Environment must resolve ${{deps.*}} tokens (env path unchanged)"
        );
    }

    // ── Contract 7: within-element repetition + multi-element independent resolve ──

    /// Contract 7: `${installPath}` may appear more than once within a single arg
    /// element — both occurrences must be substituted. Also verifies that multiple
    /// arg elements each resolve independently (two-element case).
    ///
    /// Uses `Usage::EntryPointArgs` as the caller intent, matching the runtime
    /// resolution path for baked entrypoint args.
    #[test]
    fn entrypoint_args_install_path_repeats_in_one_element() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();

        // Within-element case: ${installPath} appears twice in one arg element.
        // Both occurrences must be substituted; no ${...} markers may remain.
        let template = "--prefix=${installPath}:${installPath}/bin";
        let result = resolver.resolve(template).unwrap();
        assert_eq!(
            result.matches(install.as_ref()).count(),
            2,
            "both occurrences of ${{installPath}} in a single element must be substituted; \
             template={template:?} resolved={result:?}"
        );
        assert!(
            !result.contains("${"),
            "no template markers may remain after resolution; got: {result:?}"
        );

        // Two-element case: each element must resolve independently.
        let result_root = resolver.resolve("--root=${installPath}").unwrap();
        assert_eq!(
            result_root,
            format!("--root={install}"),
            "--root=${{installPath}} must resolve to --root=<content_path>"
        );

        let result_x = resolver.resolve("${installPath}/x").unwrap();
        assert_eq!(
            result_x,
            format!("{install}/x"),
            "${{installPath}}/x must resolve to <content_path>/x"
        );
    }

    // ── classify_install_path_rooted_dir ──────────────────────────────────────

    /// A bare `${installPath}` with no trailing slash is not the
    /// `${installPath}/<rel>` shape — no `<rel>` to classify.
    #[test]
    fn classify_install_path_rooted_dir_bare_token_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("${installPath}"), None);
    }

    /// `${installPath}/` (trailing slash, nothing after) yields an empty,
    /// non-escaping relative path — the containment root itself.
    #[test]
    fn classify_install_path_rooted_dir_trailing_slash_yields_empty_rel() {
        let rel = classify_install_path_rooted_dir("${installPath}/").expect("empty rel must parse");
        assert_eq!(rel.as_path(), Path::new(""));
    }

    /// The token must lead the value; a prefix before `${installPath}` means
    /// this is not the exact single-token shape the classifier targets.
    #[test]
    fn classify_install_path_rooted_dir_prefix_mid_string_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("foo/${installPath}/bin"), None);
    }

    /// A `<rel>` that escapes the containment root via `..` is rejected —
    /// best-effort scan-scope exclusion, not an error.
    #[test]
    fn classify_install_path_rooted_dir_escaping_rel_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("${installPath}/../etc"), None);
    }

    /// Happy path: `${installPath}/bin` classifies to the relative dir `bin`.
    #[test]
    fn classify_install_path_rooted_dir_happy_path_returns_rel() {
        let rel = classify_install_path_rooted_dir("${installPath}/bin").expect("bin must parse");
        assert_eq!(rel.as_path(), Path::new("bin"));
    }

    /// C-010 — the alias classifies exactly like the bare spelling, because
    /// they are the same referent (D4). Asserted against the bare form's own
    /// answer as well as against the literal, so the pair cannot agree on a
    /// wrong value.
    ///
    /// D4 creates this hazard and D10 closes it in the same release: on `main`
    /// the classifier is `value.strip_prefix("${installPath}/")`, and
    /// `${installPath}/` is not a prefix of `${self.installPath}/bin`, so the
    /// alias silently answered `None` — the var dropped out of `bin_scan`'s
    /// scan scope and the binaries claim came out short with no diagnostic.
    #[test]
    fn classify_install_path_rooted_dir_accepts_the_self_alias() {
        let bare = classify_install_path_rooted_dir("${installPath}/bin").expect("the bare form must classify");
        let alias = classify_install_path_rooted_dir("${self.installPath}/bin").expect("the alias must classify");
        assert_eq!(alias.as_path(), Path::new("bin"));
        assert_eq!(alias.as_path(), bare.as_path(), "the alias is not observably an alias");

        let alias_root =
            classify_install_path_rooted_dir("${self.installPath}/").expect("the alias's empty rel must parse");
        assert_eq!(alias_root.as_path(), Path::new(""));
    }

    /// C-010 — a modifier-bearing token is not the classified shape.
    /// Best-effort scan-scope exclusion, matching the treatment of every other
    /// shape the classifier cannot reduce to `<install>/<rel>`.
    #[test]
    fn classify_install_path_rooted_dir_excludes_a_modifier_bearing_token() {
        assert_eq!(classify_install_path_rooted_dir("${self.installPath:posix}/bin"), None);
        assert_eq!(classify_install_path_rooted_dir("${installPath:native}/bin"), None);
    }

    /// C-010 — a value whose scan *errors* is `None`, not a panic and not a
    /// propagated error. Under D14 this is a reachable case rather than a
    /// defensive one: the classifier runs on read paths, which no longer refuse
    /// an unrecognised token.
    #[test]
    fn classify_install_path_rooted_dir_returns_none_when_the_scan_errors() {
        assert_eq!(classify_install_path_rooted_dir("${workspaceFolder}/bin"), None);
    }

    // ── The `${self.installPath}` alias (D4) ──────────────────────────────────

    /// C-007 / S-001 — the alias resolves to identical bytes in every position:
    /// alone, prefixed, suffixed, twice in one value, and mixed with the bare
    /// spelling in one value.
    #[test]
    fn the_self_alias_resolves_identically_in_every_position() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        for (bare, alias) in [
            ("${installPath}", "${self.installPath}"),
            ("prefix-${installPath}", "prefix-${self.installPath}"),
            ("${installPath}/bin", "${self.installPath}/bin"),
            (
                "${installPath}/bin:${installPath}/lib",
                "${self.installPath}/bin:${self.installPath}/lib",
            ),
        ] {
            assert_eq!(
                resolver.resolve(alias).unwrap(),
                resolver.resolve(bare).unwrap(),
                "{alias:?} must resolve to the same bytes as {bare:?}"
            );
        }

        // Both spellings in one value: the literal answer, so the pair above
        // cannot agree on a wrong one.
        let install = dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${installPath}:${self.installPath}").unwrap(),
            format!("{install}:{install}")
        );
    }

    /// C-032 — `${installPath}` and its alias are always allowed, on every
    /// surface. A gate that told them apart would make the alias observably not
    /// an alias.
    #[test]
    fn entrypoint_args_permit_both_spellings_of_the_install_path() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${self.installPath}/app.py").unwrap(),
            format!("{install}/app.py")
        );
        assert_eq!(
            resolver.resolve("${installPath}/app.py").unwrap(),
            format!("{install}/app.py")
        );
    }

    /// D9 — the capability gate's second arm. `${self.env.*}` is legal in env
    /// values only, so an entrypoint arg refuses it as `DisallowedToken`.
    ///
    /// Asserted on the variant, which is what makes it falsifiable: delete the
    /// gate and the substitution arm refuses the same input as
    /// `UndefinedSelfEnvRef` instead — an error either way, a different answer
    /// to a different question.
    #[test]
    fn entrypoint_args_reject_a_self_env_token_as_disallowed() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let err = resolver.resolve("${self.env.TOOL_HOME}/x").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DisallowedToken { token } if token == "${self.env.TOOL_HOME}"),
            "unexpected error: {err}"
        );
    }

    // ── The escape, at the resolver (D2/D3) ───────────────────────────────────

    /// C-006(a) / S-022 — the #221 authoring path. An escaped foreign token
    /// resolves to the literal bytes, byte-identical end to end. This is the
    /// only way to put a `${…}` into published metadata, and it defends against
    /// OCX alone: the consumer expands what it receives.
    #[test]
    fn an_escaped_foreign_token_resolves_to_the_literal_bytes() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        assert_eq!(
            resolver.resolve("$${workspaceFolder}/x").unwrap(),
            "${workspaceFolder}/x"
        );
    }

    /// The escape covers **recognised** bodies too, not just foreign ones — the
    /// sibling above escapes a body OCX would refuse anyway, so on its own it
    /// leaves the wider contract untested.
    ///
    /// This is a deliberate break with the pre-scanner reading. The old
    /// `DEP_TOKEN_PATTERN` was unanchored and driven by `captures_iter`, so on
    /// `$${deps.cmake.installPath}/bin` it matched the inner token at byte 1 and
    /// substituted it, leaving the `$` stranded ahead of a resolved path. R1 now
    /// consumes `$${` first and the inner token never reaches R2. Restoring the
    /// old reading would defeat the escape for exactly the bodies OCX owns,
    /// which is the ambiguity D2/D3 exists to remove.
    ///
    /// The dep context is real, so the assertion fails if the token resolves
    /// rather than passing for want of anything to resolve against.
    #[test]
    fn an_escaped_dep_token_is_literal_text_not_a_resolved_path() {
        let dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> =
            HashMap::from([(dep_name("cmake"), ctx(dep_dir.path(), "cmake"))]);
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let resolved = resolver.resolve("$${deps.cmake.installPath}/bin").unwrap();

        assert_eq!(
            resolved, "${deps.cmake.installPath}/bin",
            "an escaped recognised body must survive as literal text"
        );
        let dep_path = dep_dir.path().to_string_lossy();
        assert!(
            !resolved.contains(dep_path.as_ref()),
            "the dependency path must not appear at all: {resolved:?}"
        );
        // The old reading's exact signature: the escape half-consumed, leaving a
        // stranded `$` in front of a resolved path. Named as an absence so a
        // regression reads as itself rather than as a generic equality failure.
        assert!(
            !resolved.starts_with(&format!("${dep_path}")),
            "the pre-scanner reading has returned: {resolved:?}"
        );
    }

    // ── Render modifiers at the resolver (D5) ─────────────────────────────────

    /// C-013 — the **defaulting** seam, which `render`'s own contracts cannot
    /// see: an omitted modifier has no representation in a
    /// `render(s, RenderModifier, Host)` call, so a resolver that wrongly maps
    /// an unmodified token onto `Posix` leaves `render` identity-correct and
    /// passes C-012 green.
    ///
    /// The `assert_ne!` is what turns this into a check. Without it, a resolver
    /// that mapped everything to `Posix` would satisfy the equality on both
    /// hosts.
    #[test]
    fn an_omitted_modifier_defaults_to_native_on_both_hosts() {
        use std::path::PathBuf;

        // The one shape where `Native` and `Posix` produce different bytes.
        let install = PathBuf::from("C:\\Users\\x");
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let windows = TemplateResolver::new(&install, &contexts).with_host(Host::Windows);
        let bare_on_windows = windows.resolve("${installPath}").unwrap();
        assert_eq!(
            bare_on_windows,
            windows.resolve("${installPath:native}").unwrap(),
            "an omitted modifier must render as :native"
        );
        assert_ne!(
            bare_on_windows,
            windows.resolve("${installPath:posix}").unwrap(),
            "on Windows :posix must not agree with the default, or the default is untestable"
        );

        let unix = TemplateResolver::new(&install, &contexts).with_host(Host::Unix);
        let bare_on_unix = unix.resolve("${installPath}").unwrap();
        assert_eq!(bare_on_unix, unix.resolve("${installPath:native}").unwrap());
        assert_eq!(
            bare_on_unix,
            unix.resolve("${installPath:posix}").unwrap(),
            ":posix is the identity off Windows, so all three agree"
        );
    }

    /// C-017 / S-010 / S-011 — the bare form takes a modifier too, and renders
    /// identically to the alias. `Host::Windows` is injected so a non-identity
    /// `:posix` runs on both CI legs; the literal answer pins that the drive
    /// letter survives and no MSYS/WSL drive form is invented.
    #[test]
    fn the_bare_form_accepts_a_posix_modifier_and_matches_the_alias() {
        use std::path::PathBuf;

        let install = PathBuf::from("C:\\Users\\x");
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(&install, &contexts).with_host(Host::Windows);

        let bare = resolver.resolve("${installPath:posix}").unwrap();
        assert_eq!(bare, "C:/Users/x");
        assert_eq!(
            bare,
            resolver.resolve("${self.installPath:posix}").unwrap(),
            "the alias must render identically under the same modifier"
        );
    }

    /// C-016 — rendering composes **after** `dunce::simplified`, so no verbatim
    /// prefix ever reaches the output under any modifier.
    ///
    /// Windows-only, deliberately: `dunce::simplified` is a no-op off Windows,
    /// so an ungated version of this reds on every Linux run. Injecting a
    /// `Host` does not help — the host-conditional thing here is
    /// `dunce::simplified` itself, and making this portable would mean owning a
    /// host-parameterised reimplementation of the verbatim-prefix strip. It
    /// therefore runs only on the Windows leg of `verify-deep.yml`, never under
    /// a local `task verify`.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_prefixed_install_path_never_reaches_the_output() {
        use std::path::PathBuf;

        let install = PathBuf::from(r"\\?\C:\Users\x");
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(&install, &contexts);

        for template in ["${installPath}", "${installPath:native}", "${installPath:posix}"] {
            let resolved = resolver.resolve(template).unwrap();
            assert!(
                !resolved.contains(r"\\?\"),
                "{template} leaked a verbatim prefix: {resolved:?}"
            );
            assert!(
                !resolved.contains("//?/"),
                "{template} leaked a slash-flipped verbatim prefix: {resolved:?}"
            );
        }
    }

    // ── D8's split, at the resolver (C-027) ───────────────────────────────────

    /// C-027 — *value resolution always; every filesystem assertion on emit
    /// only.* A declared-but-uninstalled dependency must not turn a working
    /// install into exit 79 on a value nobody emits.
    ///
    /// Both legs are required and neither substitutes for the other: the
    /// existence-free leg alone passes if the assertion is deleted outright,
    /// and the emitting leg alone says nothing about the split. Same fixture,
    /// same template, two entry points.
    #[test]
    fn a_declared_but_uninstalled_dependency_fails_only_where_the_value_is_emitted() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-there");
        let mut contexts = HashMap::new();
        contexts.insert(
            dep_name("x"),
            DependencyContext::path_only(pinned("x"), missing.clone()),
        );
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.x.installPath}/bin").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DependencyNotInstalled { ref_name, .. } if ref_name.as_str() == "x"),
            "an emitted value must still assert the dependency is installed: {err}"
        );

        assert_eq!(
            resolver
                .resolve_without_existence_checks("${deps.x.installPath}/bin")
                .unwrap(),
            format!("{}/bin", missing.to_string_lossy()),
            "a value that is not emitted must resolve without the existence assertion"
        );
    }

    // ── `${self.env.KEY}` scope lookup (D6/D7) ────────────────────────────────
    //
    // The scope is what the composer's private per-package accumulator holds:
    // the entries this package's strictly-earlier vars already resolved to, in
    // declaration order. Only `key` and `value` participate in the lookup, so
    // these fixtures say nothing about the kind.

    /// The `${self.env.KEY}` scope, built the way the composer builds it: the
    /// entries earlier vars already resolved to, in declaration order.
    fn scope_of<const N: usize>(declared: [Entry; N]) -> SelfEnvScope<Entry> {
        declared.into_iter().collect()
    }

    fn resolved(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_string(),
            value: value.to_string(),
            kind: crate::package::metadata::env::modifier::ModifierKind::Constant,
            separator: None,
        }
    }

    /// C-018 (resolver leg) — a `${self.env.KEY}` token substitutes the
    /// referenced entry's resolved value, at the position the token occupied.
    #[test]
    fn a_self_env_token_substitutes_the_referenced_entrys_value() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A", "alpha")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        assert_eq!(resolver.resolve("${self.env.A}/x").unwrap(), "alpha/x");
    }

    /// C-019 / C-020 (resolver leg) — a key the scope does not carry is
    /// undefined, and the refusal names both the key and what was in scope.
    ///
    /// Forward references and self references are the same fault seen twice:
    /// neither is in the strictly-earlier prefix, so there is no back-edge and
    /// no cycle to detect. Which document shape produces the empty prefix is
    /// the composer's contract (C-020, C-025), not this one's.
    #[test]
    fn a_self_env_token_naming_a_key_outside_the_scope_is_undefined() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A", "alpha")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        let error = resolver.resolve("${self.env.B}").unwrap_err();
        let TemplateError::UndefinedSelfEnvRef { key, declared_before } = &error else {
            panic!("expected UndefinedSelfEnvRef, got: {error}");
        };
        assert_eq!(key, "B", "the refusal must name the key that was referenced");
        assert_eq!(
            declared_before,
            &vec!["A".to_string()],
            "the refusal must list what the scope did carry, so the publisher can see the ordering"
        );
    }

    /// C-021 (resolver leg) — a key the scope carries twice is refused, not
    /// picked. Both candidates are legally visible and neither is privileged.
    ///
    /// The discriminating sibling is
    /// `a_self_env_token_substitutes_the_referenced_entrys_value`: one earlier
    /// declaration resolves, so this is not "every reference is ambiguous".
    #[test]
    fn a_self_env_token_naming_a_key_twice_in_scope_is_ambiguous() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A", "first"), resolved("A", "second")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        let error = resolver.resolve("${self.env.A}").unwrap_err();
        assert!(
            matches!(&error, TemplateError::AmbiguousSelfEnvRef { key } if key == "A"),
            "a duplicated key must be refused rather than resolved to one of the two: {error}"
        );
    }

    /// The scope is indexed as it is built rather than walked per token, so
    /// every answer must stay a function of the whole scope and not of where in
    /// it a key sits.
    ///
    /// A duplicate separated by an unrelated declaration is the shape an index
    /// gets wrong: a last-wins insert reports the pair as a single declaration,
    /// and a count kept per position resolves the key beside it to the wrong
    /// entry. The undefined leg pins the ordered listing, which is the one
    /// thing a map cannot supply on its own.
    #[test]
    fn the_self_env_scope_answers_the_same_wherever_a_key_sits() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([
            resolved("A", "first"),
            resolved("B", "beta"),
            resolved("A", "second"),
            resolved("C", "gamma"),
        ]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        for (template, expected) in [("${self.env.B}", "beta"), ("${self.env.C}", "gamma")] {
            assert_eq!(
                resolver.resolve(template).unwrap(),
                expected,
                "a singly-declared key resolves whichever side of a duplicate it sits on"
            );
        }

        let error = resolver.resolve("${self.env.A}").unwrap_err();
        assert!(
            matches!(&error, TemplateError::AmbiguousSelfEnvRef { key } if key == "A"),
            "a non-adjacent duplicate is still a duplicate: {error}"
        );

        let error = resolver.resolve("${self.env.MISSING}").unwrap_err();
        assert!(
            matches!(&error, TemplateError::UndefinedSelfEnvRef { declared_before, .. }
                if declared_before.as_slice() == ["A", "B", "A", "C"]),
            "the undefined message lists every declaration, in declaration order: {error}"
        );
    }

    /// CWE-117/150 — a declared key reaches this message straight off
    /// `metadata.json` with no grammar applied, so the escaping the token
    /// echoes already get has to cover it too.
    ///
    /// The two assertions are not redundant: absence of the raw bytes alone
    /// would also hold for an implementation that dropped the key entirely, and
    /// presence of the escaped form alone would hold for one that listed the key
    /// twice, once each way.
    #[test]
    fn a_declared_key_carrying_control_bytes_is_escaped_in_the_undefined_message() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("\u{1b}[2K\nERROR forged", "alpha")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        let rendered = resolver.resolve("${self.env.MISSING}").unwrap_err().to_string();

        assert!(
            !rendered.contains('\n') && !rendered.contains('\u{1b}'),
            "no raw control byte may reach the message: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u{1b}[2K\\nERROR forged"),
            "the key must still be named, in escaped form: {rendered:?}"
        );
    }

    /// Quoting each entry is only unambiguous because `escape_debug` escapes the
    /// delimiter too — asserted here rather than assumed, since the whole safety
    /// of the quoting rests on it.
    ///
    /// The absent-raw-form assertion is the one that discriminates: the escaped
    /// form's presence alone would also hold for a render that emitted both.
    #[test]
    fn a_declared_key_containing_a_quote_cannot_close_its_own_entry() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A'B", "alpha")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        let rendered = resolver.resolve("${self.env.MISSING}").unwrap_err().to_string();

        assert!(
            rendered.contains(r"'A\'B'"),
            "the quote in a declared key must be escaped inside its entry: {rendered:?}"
        );
        assert!(
            !rendered.contains("A'B"),
            "an unescaped quote would let the key close its own entry: {rendered:?}"
        );
    }

    /// The list is publisher-controlled in length as well as content: one entry
    /// per declared var, unbounded, is a message as long as the author cares to
    /// make it.
    ///
    /// Asserting the marker *and* the elided key's absence is what separates a
    /// bounded list from one that merely appends a marker to the whole thing.
    #[test]
    fn a_scope_past_the_listing_cap_renders_a_bounded_message() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let keys: Vec<String> = (0..MAX_LISTED_DECLARED_KEYS + 4).map(|n| format!("K{n}")).collect();
        let scope: SelfEnvScope<Entry> = keys.iter().map(|key| resolved(key, "value")).collect();
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        let error = resolver.resolve("${self.env.MISSING}").unwrap_err();
        let TemplateError::UndefinedSelfEnvRef { declared_before, .. } = &error else {
            panic!("expected UndefinedSelfEnvRef, got: {error}");
        };

        assert_eq!(
            declared_before.len(),
            MAX_LISTED_DECLARED_KEYS + 1,
            "the listing must be capped, plus one slot for the elision marker: {declared_before:?}"
        );
        assert_eq!(
            declared_before.last().map(String::as_str),
            Some(scanner::TRUNCATION_MARKER),
            "an elided listing must say so: {declared_before:?}"
        );
        let elided = keys.last().unwrap();
        assert!(
            !declared_before.contains(elided),
            "a key past the cap must not be listed: {declared_before:?}"
        );

        // The marker is ours, not the publisher's, so it renders bare where a
        // declared key renders quoted. Without this the render's marker branch
        // has no coverage at all.
        let rendered = error.to_string();
        assert!(
            rendered.ends_with("'K15', …]"),
            "the elision marker must render bare, after the last quoted key: {rendered:?}"
        );
    }

    /// Every refusal in the `${self.env.*}` family is malformed input (65) —
    /// the same class as every other metadata refusal, and distinct from
    /// `DependencyNotInstalled`'s 79.
    #[test]
    fn self_env_refusals_exit_with_data_error() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A", "first"), resolved("A", "second")]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        for template in ["${self.env.MISSING}", "${self.env.A}"] {
            let error = resolver.resolve(template).unwrap_err();
            assert_eq!(error.classify(), Some(ExitCode::DataError), "for {template}");
        }
    }

    // ── The resolved-value budget ─────────────────────────────────────────────

    /// A `${self.env.KEY}` chain that names the previous var twice doubles per
    /// var, because what is substituted is the referenced var's *resolved*
    /// value. The authored template stays 30 bytes at every step, so the
    /// publish gate — which never resolves — cannot see it; the budget is what
    /// stops it, at the first var that exceeds it rather than after the
    /// document.
    ///
    /// Each step is built the way the composer builds it: resolve against the
    /// accumulated scope, push the resolved entry, resolve the next.
    #[test]
    fn a_doubling_self_env_chain_is_refused_by_the_byte_budget() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let mut scope = scope_of([resolved("A0", &"x".repeat(8))]);
        for step in 1..=32usize {
            let previous = format!("A{}", step - 1);
            let template = format!("${{self.env.{previous}}}${{self.env.{previous}}}");
            let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

            match resolver.resolve(&template) {
                Ok(value) => scope.push(resolved(&format!("A{step}"), &value)),
                Err(error) => {
                    assert!(
                        matches!(&error, TemplateError::ResolvedValueTooLarge { limit }
                            if *limit == MAX_RESOLVED_VALUE_BYTES),
                        "a doubling chain must be refused by the budget, not by something else: {error}"
                    );
                    assert_eq!(
                        error.classify(),
                        Some(ExitCode::DataError),
                        "an over-budget value is malformed input, the same class as every other refusal"
                    );
                    assert!(
                        step <= 20,
                        "the refusal must arrive while the value is still small; it took {step} steps"
                    );
                    return;
                }
            }
        }

        panic!("a chain doubling 8 bytes per step must exceed {MAX_RESOLVED_VALUE_BYTES} bytes");
    }

    /// The budget bounds the resolved value, and its boundary is where it says
    /// it is: a value landing exactly on the ceiling resolves, one byte more
    /// does not.
    ///
    /// Without the passing leg the check above is satisfied by a resolver that
    /// refuses everything; without the one-byte leg, by a budget an order of
    /// magnitude off.
    #[test]
    fn a_value_at_the_budget_resolves_and_one_byte_more_does_not() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let scope = scope_of([resolved("A", &"x".repeat(MAX_RESOLVED_VALUE_BYTES))]);
        let resolver = TemplateResolver::new(dir.path(), &contexts).with_self_env(&scope);

        assert_eq!(
            resolver.resolve("${self.env.A}").unwrap().len(),
            MAX_RESOLVED_VALUE_BYTES,
            "a value exactly at the ceiling must still resolve"
        );

        let error = resolver
            .resolve("${self.env.A}x")
            .expect_err("one byte over the ceiling must be refused");
        assert!(
            matches!(&error, TemplateError::ResolvedValueTooLarge { .. }),
            "unexpected error: {error}"
        );
    }
}
