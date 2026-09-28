// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Env-package composition: layers, entrypoint synthesis, interpreter dependency, env metadata.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde_json::json;
use uv_distribution_filename::WheelFilename;

use ocx_oci::{LayerLayoutSpec, Platform};
use ocx_package::metadata::Metadata;
use ocx_package::metadata::binary::Binaries;
use ocx_package::metadata::bundle::{Bundle, Version as BundleVersion};
use ocx_package::metadata::dependency::Dependencies;
use ocx_package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};
use ocx_package::metadata::env::EnvBuilder;
use ocx_package::metadata::integrations::Integrations;

use crate::naming::normalize_package_name;
use crate::platform::{PythonTarget, TargetArchitecture, TargetOperatingSystem, TargetPlatform};
use crate::repack::RepackedWheel;

/// Which wheels' `[console_scripts]` entries synthesize as entrypoints.
///
/// Under [`RootOnly`](Self::RootOnly) an app that spawns a dependency's console script finds nothing unless it is
/// admitted via [`All`](Self::All) or [`Explicit`](Self::Explicit).
#[derive(Debug, Clone)]
pub enum EntrypointSelection {
    /// Only the root package's own console scripts synthesize. The default.
    RootOnly {
        /// The root package's dist name, normalized here before comparing.
        root_package: String,
    },
    /// Every wheel's console scripts synthesize.
    All,
    /// Only the listed console-script names synthesize; an unprovided name is [`ComposeError::MissingEntrypoint`].
    Explicit(Vec<String>),
}

fn entrypoint_admitted(selection: &EntrypointSelection, wheel_dist_name: &str, script_name: &str) -> bool {
    match selection {
        EntrypointSelection::All => true,
        EntrypointSelection::RootOnly { root_package } => wheel_dist_name == normalize_package_name(root_package),
        EntrypointSelection::Explicit(names) => names.iter().any(|name| name == script_name),
    }
}

/// Consumer-declared inputs to composition.
#[derive(Debug, Clone)]
pub struct EnvSpec {
    /// The extras requested for this env; each must appear in [`declared_extras`](Self::declared_extras).
    pub requested_extras: Vec<String>,
    /// The extras the lock declares (its top-level `extras` key).
    pub declared_extras: Vec<String>,
    /// The private interpreter dependency, whose `python` every synthesized entrypoint runs.
    pub interpreter: ocx_package::metadata::dependency::Dependency,
    /// The selection target: the base os/arch platform and the ABI the wheels are checked against.
    pub target: PythonTarget,
    /// Which wheels' console scripts synthesize as entrypoints.
    pub entrypoint_selection: EntrypointSelection,
}

/// A single wheel layer descriptor: its source layer plus placement.
#[derive(Debug, Clone)]
pub struct WheelLayer {
    /// Path to the repacked `tar.zst` layer.
    pub source: PathBuf,
    /// The per-layer strip and prefix; empty, since `repack` already emitted the final tree.
    pub layout: LayerLayoutSpec,
}

/// The target-agnostic composition of an env package.
#[derive(Debug, Clone)]
pub struct EnvComposition {
    /// The composed bundle metadata.
    pub metadata: Metadata,
    /// The featureless base os/arch platform; the consumer owns the published platform key.
    pub platform: Platform,
    /// The ordered wheel layer descriptors.
    pub layers: Vec<WheelLayer>,
}

impl EnvComposition {
    /// Assembles the final [`Info`](ocx_package::info::Info), which names no registry.
    pub fn into_info(self) -> ocx_package::info::Info {
        ocx_package::info::Info {
            metadata: self.metadata,
            platform: self.platform,
        }
    }
}

/// Composes an env package from a validated wheel set and consumer inputs.
///
/// # Errors
///
/// [`ComposeError::UnknownExtra`] for an undeclared requested extra, [`ComposeError::AbiMismatch`] for a wheel
/// inconsistent with the target ABI, [`ComposeError::InvalidEntryPoint`] for a malformed object reference or name,
/// [`ComposeError::EntrypointCollision`] when two wheels claim one name, and [`ComposeError::MissingEntrypoint`]
/// for an `Explicit` name no wheel provides.
pub fn compose_env(spec: &EnvSpec, wheels: &[RepackedWheel]) -> Result<EnvComposition, ComposeError> {
    for extra in &spec.requested_extras {
        if !spec.declared_extras.contains(extra) {
            return Err(ComposeError::UnknownExtra { extra: extra.clone() });
        }
    }

    let interpreter_abi = spec.target.effective_abi();
    for wheel in wheels {
        check_abi(&wheel.filename, interpreter_abi)?;
    }

    let mut entries: BTreeMap<EntrypointName, Entrypoint> = BTreeMap::new();
    let mut claimed_by: BTreeMap<EntrypointName, String> = BTreeMap::new();
    let mut matched_explicit_names: HashSet<&str> = HashSet::new();
    for wheel in wheels {
        let wheel_dist_name = wheel
            .filename
            .parse::<WheelFilename>()
            .expect("check_abi already validated this wheel's filename parses")
            .name
            .to_string();

        for script in &wheel.entry_points {
            if !script.extras.iter().all(|extra| spec.requested_extras.contains(extra)) {
                continue;
            }
            if !entrypoint_admitted(&spec.entrypoint_selection, &wheel_dist_name, &script.name) {
                continue;
            }
            if let EntrypointSelection::Explicit(_) = &spec.entrypoint_selection {
                matched_explicit_names.insert(script.name.as_str());
            }
            // Validated before `synthesize_shim`, which embeds the name unescaped in a Python literal.
            let name = EntrypointName::try_from(script.name.as_str()).map_err(|_| ComposeError::InvalidEntryPoint {
                name: script.name.clone(),
                reference: script.reference.clone(),
            })?;
            if let Some(first_wheel) = claimed_by.get(&name) {
                return Err(ComposeError::EntrypointCollision {
                    name: script.name.clone(),
                    first_wheel: first_wheel.clone(),
                    second_wheel: wheel.filename.clone(),
                });
            }
            let shim = synthesize_shim(script.name.as_str(), &script.reference).ok_or_else(|| {
                ComposeError::InvalidEntryPoint {
                    name: script.name.clone(),
                    reference: script.reference.clone(),
                }
            })?;
            // `python`, not `python3`: on Windows `python3` hits the WindowsApps store-alias stub, which hangs.
            let entrypoint: Entrypoint = serde_json::from_value(json!({
                "command": "python",
                "args": ["-c", shim],
            }))
            .expect("python is a valid entrypoint command and the shim args are strings");
            claimed_by.insert(name.clone(), wheel.filename.clone());
            entries.insert(name, entrypoint);
        }
    }

    if let EntrypointSelection::Explicit(names) = &spec.entrypoint_selection {
        for name in names {
            if !matched_explicit_names.contains(name.as_str()) {
                return Err(ComposeError::MissingEntrypoint { name: name.clone() });
            }
        }
    }

    // A bare `python` entrypoint, or a spec's test script runs the host's interpreter (absent on many images).
    // `or_default`, or a wheel's own `python` script is overwritten. Cannot recurse: `python` resolves on the SELF-view
    // PATH, which `Entrypoints::IMPLICIT_VISIBILITY` keeps `entrypoints/` off.
    entries
        .entry(EntrypointName::try_from("python").expect("`python` matches the entrypoint-name slug pattern"))
        .or_default();

    let entrypoints = Entrypoints::new(entries);

    let env = EnvBuilder::new()
        .with_path("PYTHONPATH", "${installPath}/lib/site-packages", true)
        // Optional: a pure-python app ships no `bin/`, and `required` fails it with exit 79.
        .with_path("PATH", "${installPath}/bin", false)
        // Keeps `__pycache__` writes out of read-only package content.
        .with_constant("PYTHONDONTWRITEBYTECODE", "1")
        .build();

    let dependencies = Dependencies::new(vec![spec.interpreter.clone()])
        .expect("a single interpreter dependency cannot duplicate an identifier or name");

    let platform = base_platform(&spec.target.platform);

    let layers = wheels
        .iter()
        .map(|wheel| WheelLayer {
            source: wheel.layer_path.clone(),
            layout: LayerLayoutSpec::default(),
        })
        .collect();

    // `Some([])`, not `None` ("never scanned"), or every closure holding this env reports `binaries incomplete`.
    let bundle = Bundle {
        version: BundleVersion::V1,
        strip_components: None,
        env,
        dependencies,
        entrypoints,
        binaries: Some(Binaries::default()),
        integrations: Integrations::default(),
    };

    Ok(EnvComposition {
        metadata: Metadata::Bundle(bundle),
        platform,
        layers,
    })
}

/// Synthesizes the `python -c` shim for a `module[:attr[.attr…]]` object reference; `None` when it is malformed.
///
/// `name` is embedded unescaped in a Python string literal, so the caller must have validated it as an
/// `EntrypointName`.
fn synthesize_shim(name: &str, reference: &str) -> Option<String> {
    let (module, attrs) = parse_object_reference(reference)?;
    let mut lines = vec![
        "import importlib, sys".to_string(),
        // Without this, click and argparse print `-c` as the program name in usage and --help.
        format!("sys.argv[0] = \"{name}\""),
        format!("_obj = importlib.import_module(\"{module}\")"),
    ];
    for attr in attrs {
        lines.push(format!("_obj = getattr(_obj, \"{attr}\")"));
    }
    lines.push("sys.exit(_obj())".to_string());
    Some(lines.join("\n"))
}

/// Parses an object reference `module[:attr[.attr…]]`; `None` when malformed.
///
/// Every segment must be a Python identifier, since the shim embeds them unescaped.
fn parse_object_reference(reference: &str) -> Option<(&str, Vec<&str>)> {
    let (module, attr_chain) = match reference.split_once(':') {
        Some((module, attrs)) => (module, Some(attrs)),
        None => (reference, None),
    };
    if !is_valid_dotted_identifier(module) {
        return None;
    }
    let attrs = match attr_chain {
        None => Vec::new(),
        Some(chain) if chain.contains(':') || !is_valid_dotted_identifier(chain) => return None,
        Some(chain) => chain.split('.').collect(),
    };
    Some((module, attrs))
}

fn is_valid_dotted_identifier(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_valid_python_identifier)
}

/// ASCII-only Python identifier check.
fn is_valid_python_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Admits `none`, `abi3` or an ABI equal to the interpreter's; an unparseable filename is rejected too.
fn check_abi(filename: &str, interpreter_abi: &str) -> Result<(), ComposeError> {
    let wheel_abis: Vec<String> = match filename.parse::<WheelFilename>() {
        Ok(wheel) => wheel.abi_tags().iter().map(ToString::to_string).collect(),
        Err(_) => {
            return Err(ComposeError::AbiMismatch {
                filename: filename.to_string(),
                wheel_abi: "unparseable".to_string(),
                interpreter_abi: interpreter_abi.to_string(),
            });
        }
    };
    let compatible = wheel_abis
        .iter()
        .any(|abi| abi == "none" || abi == "abi3" || abi == interpreter_abi);
    if compatible {
        Ok(())
    } else {
        Err(ComposeError::AbiMismatch {
            filename: filename.to_string(),
            wheel_abi: wheel_abis.join("."),
            interpreter_abi: interpreter_abi.to_string(),
        })
    }
}

/// Errors from env-package composition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ComposeError {
    /// A requested extra is not declared in the lock's top-level `extras`.
    #[error("requested extra '{extra}' is not declared in the lock")]
    UnknownExtra {
        /// The undeclared extra.
        extra: String,
    },
    /// A wheel's ABI is inconsistent with the interpreter pin.
    #[error("wheel '{filename}' ABI '{wheel_abi}' is incompatible with interpreter ABI '{interpreter_abi}'")]
    AbiMismatch {
        /// The offending wheel filename.
        filename: String,
        /// The wheel's ABI tag.
        wheel_abi: String,
        /// The interpreter's ABI tag.
        interpreter_abi: String,
    },
    /// A `[console_scripts]` object reference does not parse as
    /// `module[:attr[.attr…]]`.
    #[error("invalid entry point '{name}': '{reference}' is not a valid object reference")]
    InvalidEntryPoint {
        /// The entry-point name.
        name: String,
        /// The malformed object reference.
        reference: String,
    },
    /// Two different wheels registered a console script under the same entrypoint name.
    #[error("entrypoint '{name}' is registered by both '{first_wheel}' and '{second_wheel}'")]
    EntrypointCollision {
        /// The colliding entrypoint name.
        name: String,
        /// The wheel filename that first claimed `name`.
        first_wheel: String,
        /// The wheel filename that claimed `name` again.
        second_wheel: String,
    },
    /// An [`EntrypointSelection::Explicit`] name matched no admitted wheel's console script.
    #[error("entrypoint '{name}' was requested but not found in any wheel")]
    MissingEntrypoint {
        /// The requested-but-absent entrypoint name.
        name: String,
    },
}

/// Maps a target's os/arch key to a featureless OCX [`Platform`].
fn base_platform(platform: &TargetPlatform) -> Platform {
    use ocx_oci::{Architecture, OperatingSystem};

    let os = match platform.operating_system {
        TargetOperatingSystem::Linux => OperatingSystem::Linux,
        TargetOperatingSystem::Darwin => OperatingSystem::Darwin,
        TargetOperatingSystem::Windows => OperatingSystem::Windows,
    };
    let arch = match platform.architecture {
        TargetArchitecture::Amd64 => Architecture::Amd64,
        TargetArchitecture::Arm64 => Architecture::Arm64,
    };
    Platform::Specific {
        os,
        arch,
        variant: None,
        os_features: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_package::metadata::dependency::Dependency;

    use crate::platform::{
        Implementation, InterpreterPin, TargetArchitecture, TargetOperatingSystem, TargetPlatform, VariantConstraints,
    };
    use crate::repack::ConsoleScript;

    // ── Inline construction helpers (no fixtures) ───────────────────────────

    fn interpreter_dependency() -> Dependency {
        let json = format!(r#"{{"identifier":"ocx.sh/python:3.13@sha256:{}"}}"#, "a".repeat(64));
        serde_json::from_str(&json).expect("interpreter dependency parses")
    }

    fn python_target(abi: &str) -> PythonTarget {
        PythonTarget {
            platform: TargetPlatform {
                operating_system: TargetOperatingSystem::Linux,
                architecture: TargetArchitecture::Amd64,
            },
            variant: VariantConstraints::default(),
            interpreter: InterpreterPin {
                python_version: "3.13".to_string(),
                python_full_version: "3.13.1".to_string(),
                abi: abi.to_string(),
                implementation: Implementation::CPython,
            },
        }
    }

    /// Builds an [`EnvSpec`] with [`EntrypointSelection::All`] — the prior
    /// unconditional-synthesis behavior — so tests that don't exercise
    /// selection modes keep asserting on every wheel's scripts, unchanged.
    fn env_spec(requested: &[&str], declared: &[&str], abi: &str) -> EnvSpec {
        EnvSpec {
            requested_extras: requested.iter().map(ToString::to_string).collect(),
            declared_extras: declared.iter().map(ToString::to_string).collect(),
            interpreter: interpreter_dependency(),
            target: python_target(abi),
            entrypoint_selection: EntrypointSelection::All,
        }
    }

    fn console_script(name: &str, reference: &str, extras: &[&str]) -> ConsoleScript {
        ConsoleScript {
            name: name.to_string(),
            reference: reference.to_string(),
            extras: extras.iter().map(ToString::to_string).collect(),
        }
    }

    fn wheel(filename: &str, scripts: Vec<ConsoleScript>) -> RepackedWheel {
        RepackedWheel {
            filename: filename.to_string(),
            layer_path: PathBuf::from(format!("/layers/{filename}.tar.zst")),
            layer_digest: format!("sha256:{}", "b".repeat(64)),
            wheel_sha256: "c".repeat(64),
            entry_points: scripts,
            record_paths: Vec::new(),
        }
    }

    /// A pure-Python wheel (`none` ABI), compatible with any interpreter — used
    /// where a test isolates entrypoint/env behaviour from the ABI check.
    const PURE_WHEEL: &str = "foo-1.0-py3-none-any.whl";

    /// Returns the console-script entrypoint `name`'s args (`["-c", shim]`).
    fn sole_entrypoint_args(composition: &EnvComposition, name: &str) -> Vec<String> {
        let entrypoints = composition
            .metadata
            .entrypoints()
            .expect("bundle metadata carries entrypoints");
        let (_, entry) = entrypoints
            .iter()
            .find(|(entry_name, _)| entry_name.as_str() == name)
            .unwrap_or_else(|| panic!("entrypoint {name} present"));
        assert_eq!(
            entry.command().expect("dispatch command set").as_str(),
            "python",
            "every synthesized entrypoint dispatches python"
        );
        let args = entry.args();
        assert_eq!(args[0], "-c", "shim runs via python -c, no shell");
        args.to_vec()
    }

    // ── Entrypoint synthesis: shim grammar ──────────────────────────────────

    #[test]
    fn simple_module_func_reference_builds_importlib_getattr_call_shim() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("mytool", "mod:func", &[])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let args = sole_entrypoint_args(&composition, "mytool");
        let shim = &args[1];

        assert!(shim.contains("importlib.import_module(\"mod\")"), "shim: {shim}");
        assert!(shim.contains("getattr(_obj, \"func\")"), "shim: {shim}");
        assert!(
            shim.contains("sys.exit(_obj())"),
            "shim calls the resolved object: {shim}"
        );
        // Regression: argv[0] must be the console-script name, not `-c`, so
        // click/argparse report the real program name in --version/--help.
        assert!(
            shim.contains("sys.argv[0] = \"mytool\""),
            "shim must set argv[0] to the entrypoint name: {shim}"
        );
        assert!(
            !shim.contains("import func"),
            "must not use a from-import template: {shim}"
        );
    }

    #[test]
    fn dotted_attr_reference_walks_each_getattr_in_order() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(
            PURE_WHEEL,
            vec![console_script("tool", "pkg.mod:Class.method", &[])],
        )];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let args = sole_entrypoint_args(&composition, "tool");
        let shim = &args[1];

        assert!(shim.contains("importlib.import_module(\"pkg.mod\")"), "shim: {shim}");
        let class_at = shim.find("getattr(_obj, \"Class\")").expect("Class getattr present");
        let method_at = shim.find("getattr(_obj, \"method\")").expect("method getattr present");
        assert!(class_at < method_at, "attr chain must walk Class before method: {shim}");
    }

    #[test]
    fn module_only_reference_imports_without_getattr() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("flask", "flask.cli", &[])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let args = sole_entrypoint_args(&composition, "flask");
        let shim = &args[1];

        assert!(shim.contains("importlib.import_module(\"flask.cli\")"), "shim: {shim}");
        assert!(!shim.contains("getattr"), "a module-only ref has no attr walk: {shim}");
        assert!(shim.contains("sys.exit(_obj())"), "shim: {shim}");
    }

    #[test]
    fn malformed_reference_is_invalid_entry_point() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("bad", "a:b:c", &[])])];

        let error = compose_env(&spec, &wheels).expect_err("a two-colon reference is malformed");
        assert!(
            matches!(error, ComposeError::InvalidEntryPoint { ref name, ref reference } if name == "bad" && reference == "a:b:c"),
            "got {error:?}"
        );
    }

    // ── Extras gating ───────────────────────────────────────────────────────

    #[test]
    fn extras_gated_script_is_skipped_when_extra_not_requested() {
        // `d` is declared but not requested → the blackd launcher is not synthesized.
        let spec = env_spec(&[], &["d"], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("blackd", "blackd:main", &["d"])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let entrypoints = composition.metadata.entrypoints().expect("entrypoints present");
        assert!(
            !entrypoints.iter().any(|(n, _)| n.as_str() == "blackd"),
            "an unrequested extra must not synthesize its launcher"
        );
    }

    #[test]
    fn library_wheel_with_no_scripts_composes_without_console_script_entrypoints() {
        // A library env (no console scripts of its own — e.g.
        // google-cloud-aiplatform) is not a compose failure; it just carries
        // nothing beyond the always-present `python`.
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, Vec::new())];
        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let names: Vec<&str> = composition
            .metadata
            .entrypoints()
            .expect("entrypoints present")
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["python"], "a library env synthesizes no console scripts");
    }

    #[test]
    fn extras_gated_script_is_synthesized_when_extra_requested() {
        let spec = env_spec(&["d"], &["d"], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("blackd", "blackd:main", &["d"])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let args = sole_entrypoint_args(&composition, "blackd");
        assert!(
            args[1].contains("importlib.import_module(\"blackd\")"),
            "shim: {}",
            args[1]
        );
    }

    #[test]
    fn requested_extra_absent_from_declared_is_unknown_extra() {
        let spec = env_spec(&["full"], &["d"], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, Vec::new())];

        let error = compose_env(&spec, &wheels).expect_err("a requested extra not declared must fail");
        assert!(
            matches!(error, ComposeError::UnknownExtra { ref extra } if extra == "full"),
            "got {error:?}"
        );
    }

    // ── ABI consistency ─────────────────────────────────────────────────────

    #[test]
    fn cp313_wheel_against_free_threaded_interpreter_is_abi_mismatch() {
        // A concrete cp313 wheel must not compose against a cp313t interpreter.
        let spec = env_spec(&[], &[], "cp313t");
        let wheels = vec![wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", Vec::new())];

        let error = compose_env(&spec, &wheels).expect_err("cp313 vs cp313t must fail closed");
        match error {
            ComposeError::AbiMismatch {
                filename,
                wheel_abi,
                interpreter_abi,
            } => {
                assert_eq!(filename, "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl");
                assert_eq!(wheel_abi, "cp313");
                assert_eq!(interpreter_abi, "cp313t");
            }
            other => panic!("expected AbiMismatch, got {other:?}"),
        }
    }

    #[test]
    fn matching_cpython_abi_composes() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", Vec::new())];
        assert!(
            compose_env(&spec, &wheels).is_ok(),
            "a cp313 wheel matches a cp313 interpreter"
        );
    }

    #[test]
    fn variant_abi_override_is_the_effective_abi_not_the_interpreter_pin() {
        // A documented free-threaded target: variant.abi overrides to cp313t
        // while the interpreter pin itself still reports cp313. compose must
        // judge wheels against the effective (variant-overridden) ABI, the same
        // one `select` used to pick them — not the raw interpreter pin.
        let mut spec = env_spec(&[], &[], "cp313");
        spec.target.variant.abi = Some("cp313t".to_string());

        let free_threaded_wheel = wheel("numpy-2.1.3-cp313-cp313t-manylinux_2_28_x86_64.whl", Vec::new());
        assert!(
            compose_env(&spec, &[free_threaded_wheel]).is_ok(),
            "a cp313t wheel must compose against a variant-overridden cp313t target"
        );

        let non_free_threaded_wheel = wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", Vec::new());
        let error = compose_env(&spec, &[non_free_threaded_wheel])
            .expect_err("a cp313 wheel must not compose against the cp313t effective ABI");
        match error {
            ComposeError::AbiMismatch { interpreter_abi, .. } => {
                assert_eq!(
                    interpreter_abi, "cp313t",
                    "must report the effective (variant-overridden) ABI, not the interpreter pin's"
                );
            }
            other => panic!("expected AbiMismatch, got {other:?}"),
        }
    }

    // ── Env block, layers, platform ─────────────────────────────────────────

    #[test]
    fn env_block_carries_pythonpath_path_and_dontwritebytecode() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, Vec::new())];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let env = composition.metadata.env().expect("bundle metadata carries env");
        let env_json = serde_json::to_string(env).expect("env serializes");

        assert!(env_json.contains("PYTHONPATH"), "env: {env_json}");
        assert!(env_json.contains("lib/site-packages"), "env: {env_json}");
        assert!(env_json.contains("PATH"), "env: {env_json}");
        assert!(
            env_json.contains("PYTHONDONTWRITEBYTECODE"),
            "runtime-write mitigation must be present: {env_json}"
        );
    }

    #[test]
    fn binaries_are_declared_empty_not_undeclared() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("mytool", "mod:func", &[])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let binaries = composition
            .metadata
            .binaries()
            .expect("an env declares its binaries claim, never leaving it undeclared");
        assert!(binaries.is_empty(), "an env ships no bin/ executables to claim");
    }

    #[test]
    fn each_wheel_becomes_a_content_root_layer_with_empty_layout() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![
            wheel("foo-1.0-py3-none-any.whl", Vec::new()),
            wheel("bar-2.0-py3-none-any.whl", Vec::new()),
        ];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        assert_eq!(composition.layers.len(), 2, "one layer per wheel");
        for layer in &composition.layers {
            assert!(
                layer.layout.is_empty(),
                "repack emits the final tree; the layer applies at the content root"
            );
        }
        assert_eq!(
            composition.layers[0].source,
            PathBuf::from("/layers/foo-1.0-py3-none-any.whl.tar.zst")
        );
    }

    #[test]
    fn platform_is_the_l2_os_arch_encoding() {
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, Vec::new())];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        assert_eq!(composition.platform.to_string(), "linux/amd64");
    }

    // ── Entrypoint selection modes ──────────────────────────────────────────

    #[test]
    fn root_only_admits_only_the_root_packages_own_scripts() {
        let mut spec = env_spec(&[], &[], "cp313");
        spec.entrypoint_selection = EntrypointSelection::RootOnly {
            root_package: "root-pkg".to_string(),
        };
        let wheels = vec![
            wheel(
                "root_pkg-1.0.0-py3-none-any.whl",
                vec![console_script("roottool", "root_pkg:main", &[])],
            ),
            wheel(
                "dep_pkg-2.0.0-py3-none-any.whl",
                vec![console_script("deptool", "dep_pkg:main", &[])],
            ),
        ];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let entrypoints = composition.metadata.entrypoints().expect("entrypoints present");
        assert!(
            entrypoints.iter().any(|(name, _)| name.as_str() == "roottool"),
            "the root package's own script must synthesize"
        );
        assert!(
            !entrypoints.iter().any(|(name, _)| name.as_str() == "deptool"),
            "a dependency's script must NOT synthesize under RootOnly (the new default)"
        );
    }

    #[test]
    fn root_only_normalizes_the_configured_root_package_name() {
        // The spec's root_package is a raw, un-normalized string; compose must
        // PEP-503-normalize it before comparing against the wheel's parsed
        // (already-normalized) dist name.
        let mut spec = env_spec(&[], &[], "cp313");
        spec.entrypoint_selection = EntrypointSelection::RootOnly {
            root_package: "Root.PKG".to_string(),
        };
        let wheels = vec![wheel(
            "root_pkg-1.0.0-py3-none-any.whl",
            vec![console_script("roottool", "root_pkg:main", &[])],
        )];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let entrypoints = composition.metadata.entrypoints().expect("entrypoints present");
        assert!(
            entrypoints.iter().any(|(name, _)| name.as_str() == "roottool"),
            "a differently-cased/separated root_package must still normalize-match the wheel"
        );
    }

    #[test]
    fn explicit_admits_only_named_scripts() {
        let mut spec = env_spec(&[], &[], "cp313");
        spec.entrypoint_selection = EntrypointSelection::Explicit(vec!["foo".to_string()]);
        let wheels = vec![wheel(
            PURE_WHEEL,
            vec![
                console_script("foo", "mod:foo", &[]),
                console_script("bar", "mod:bar", &[]),
            ],
        )];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let entrypoints = composition.metadata.entrypoints().expect("entrypoints present");
        assert!(entrypoints.iter().any(|(name, _)| name.as_str() == "foo"));
        assert!(
            !entrypoints.iter().any(|(name, _)| name.as_str() == "bar"),
            "an unlisted script must not synthesize under Explicit"
        );
    }

    #[test]
    fn explicit_name_absent_from_every_wheel_is_missing_entrypoint_error() {
        let mut spec = env_spec(&[], &[], "cp313");
        spec.entrypoint_selection = EntrypointSelection::Explicit(vec!["ghost".to_string()]);
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("foo", "mod:foo", &[])])];

        let error = compose_env(&spec, &wheels).expect_err("a requested-but-absent name must fail closed");
        assert!(
            matches!(error, ComposeError::MissingEntrypoint { ref name } if name == "ghost"),
            "got {error:?}"
        );
    }

    // ── The always-present `python` entrypoint ──────────────────────────────

    #[test]
    fn python_entrypoint_is_always_synthesized_alongside_console_scripts() {
        // Regression: without this entry the composed package ships no `python`
        // launcher, so a test script's bare `python` resolves to the HOST
        // interpreter under `ocx package test` (or fails to spawn at all on a
        // container image that has none).
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("mytool", "mod:func", &[])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let entrypoints = composition.metadata.entrypoints().expect("entrypoints present");
        let (_, python) = entrypoints
            .iter()
            .find(|(name, _)| name.as_str() == "python")
            .expect("every env carries a `python` entrypoint");
        // No `command`, so `Entrypoints::dispatch_command` returns the name
        // verbatim and `ocx launcher exec` resolves it on the self-view PATH —
        // where the root's own `entrypoints/` is absent and the private
        // interpreter's `bin/` is present. A `command: python` would resolve
        // identically; leaving it unset is what makes the no-recursion
        // property structural rather than incidental.
        assert!(
            python.command().is_none(),
            "the `python` entrypoint dispatches its own name, not a divergent command"
        );
        assert!(
            python.args().is_empty(),
            "the `python` entrypoint bakes no args — user argv passes through verbatim"
        );
        assert!(
            entrypoints.iter().any(|(name, _)| name.as_str() == "mytool"),
            "synthesizing `python` must not displace a console script"
        );
        assert!(
            !entrypoints.iter().any(|(name, _)| name.as_str() == "python3"),
            "only `python` is synthesized: python-build-standalone has no `python3` on Windows"
        );
    }

    #[test]
    fn wheel_declared_python_console_script_is_not_clobbered() {
        // A wheel that genuinely ships a `python` console script keeps its own
        // shim; the synthesized fallback must never overwrite it.
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![wheel(PURE_WHEEL, vec![console_script("python", "mod:main", &[])])];

        let composition = compose_env(&spec, &wheels).expect("composition succeeds");
        let args = sole_entrypoint_args(&composition, "python");
        assert!(
            args[1].contains("importlib.import_module(\"mod\")"),
            "a wheel-declared `python` script keeps its own shim: {}",
            args[1]
        );
    }

    #[test]
    fn cross_wheel_name_collision_is_entrypoint_collision_error() {
        // Two different wheels registering the same console-script name under
        // an admitting mode (`All`) must fail closed, not silently keep
        // whichever wheel composed last.
        let spec = env_spec(&[], &[], "cp313");
        let wheels = vec![
            wheel(
                "first_pkg-1.0.0-py3-none-any.whl",
                vec![console_script("same", "first_pkg:main", &[])],
            ),
            wheel(
                "second_pkg-1.0.0-py3-none-any.whl",
                vec![console_script("same", "second_pkg:main", &[])],
            ),
        ];

        let error = compose_env(&spec, &wheels).expect_err("a cross-wheel name clash must fail closed");
        match error {
            ComposeError::EntrypointCollision {
                name,
                first_wheel,
                second_wheel,
            } => {
                assert_eq!(name, "same");
                assert_eq!(first_wheel, "first_pkg-1.0.0-py3-none-any.whl");
                assert_eq!(second_wheel, "second_pkg-1.0.0-py3-none-any.whl");
            }
            other => panic!("expected EntrypointCollision, got {other:?}"),
        }
    }
}
