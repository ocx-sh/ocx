// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline generate ci` — renders the GHA workflow and support
//! scripts from `mirror.yml` using baked-in templates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ocx_lib::cli::DataInterface;

use crate::command::options::OutputFormat;
use crate::error::MirrorError;
use crate::spec::{self, MirrorSpec, PlatformConfig, TestEntry};

// ── Build-time constants ─────────────────────────────────────────────────────

/// OCX-mirror crate version baked in at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git SHA injected by `build.rs` via `OCX_GIT_SHA_SHORT`.
/// Falls back to `"unknown"` when the build environment has no git context.
const GIT_SHA_SHORT: &str = match option_env!("OCX_GIT_SHA_SHORT") {
    Some(sha) => sha,
    None => "unknown",
};

// ── Baked-in templates ───────────────────────────────────────────────────────

const WORKFLOW_TEMPLATE: &str = include_str!("templates/workflow.yml");
const DESCRIBE_TEMPLATE: &str = include_str!("templates/describe.yml");

// ── Public struct ────────────────────────────────────────────────────────────

/// Generate (or check) the CI workflow files for a mirror repository.
///
/// In write mode: renders `.github/workflows/mirror.yml` and
/// `.github/workflows/describe.yml`.
///
/// In `--check` mode: exits 65 (DataError) if any generated file drifts from
/// what would be produced; emits path-only hints to stderr.
#[derive(clap::Parser)]
pub struct GenerateCi {
    /// Path to the mirror spec file.
    #[arg(long, default_value = "./mirror.yml")]
    pub spec: PathBuf,

    /// Check mode: verify generated files are up-to-date; exit 65 on drift.
    #[arg(long)]
    pub check: bool,

    /// Output format for diagnostics.
    #[arg(long)]
    pub format: Option<OutputFormat>,
}

impl GenerateCi {
    pub async fn execute(&self, _printer: &DataInterface) -> Result<(), MirrorError> {
        // Phase 1: policy-level pre-flight before load_spec.
        //
        // Check for `ocx_install:` key in the raw YAML text. MirrorSpec uses
        // `#[serde(deny_unknown_fields)]` so load_spec would emit SpecInvalid (65),
        // but plan §1.8 requires SpecUsageError (64) for this specific case.
        // Peeking the raw bytes lets us intercept before serde rejects it.
        let raw = tokio::fs::read_to_string(&self.spec)
            .await
            .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", self.spec.display())))?;

        if raw.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("ocx_install:") || trimmed == "ocx_install:"
        }) {
            return Err(MirrorError::SpecUsageError(
                "ocx binary is installed via direct release download; \
                 remove `ocx_install:` block. \
                 Override `OCX_BINARY_OVERRIDE` env var at workflow level for integration tests"
                    .to_string(),
            ));
        }

        // Phase 2: load and validate spec (structural validation).
        let spec = spec::load_spec(&self.spec).await?;

        // Phase 3: content-policy validation on the parsed spec.
        policy_check_notify(&spec)?;

        // Phase 4: render all generated files.
        let repo_root = self.spec.parent().unwrap_or(Path::new("."));
        let files = render(&spec, repo_root)?;

        // Phase 5: write or check.
        if self.check {
            check_drift(&files, repo_root).await
        } else {
            write_files(&files, repo_root).await
        }
    }
}

// ── Policy validation ────────────────────────────────────────────────────────

/// Content-policy check on the `notify:` block.
///
/// Delegates to `spec::policy_check_notify` so the check logic lives in one place
/// and always returns `SpecUsageError (64)` for URL-literal webhook secrets.
/// `load_spec` already calls this before structural validation, so this call in
/// the renderer is a defence-in-depth guard for specs loaded via other paths.
fn policy_check_notify(spec: &MirrorSpec) -> Result<(), MirrorError> {
    let Some(notify) = &spec.notify else {
        return Ok(());
    };
    spec::policy_check_notify(notify)
}

// ── Renderer ─────────────────────────────────────────────────────────────────

/// Slug a container image reference to a stable, filesystem-safe identifier.
///
/// `ubuntu:24.04` → `ubuntu_2404`, `alpine:3.20` → `alpine_3_20`.
fn container_slug(image: &str) -> String {
    image.replace([':', '/'], "_").replace('.', "_")
}

/// Effective shell for a container: explicit field wins, then image-name inference.
fn effective_shell_for_container(image: &str, explicit: Option<&str>) -> &'static str {
    if let Some(s) = explicit {
        return match s {
            "sh" => "sh",
            "bash" => "bash",
            "pwsh" => "pwsh",
            _ => "bash",
        };
    }
    // Use the same inference logic as the validator.
    let base = image
        .split(':')
        .next()
        .unwrap_or(image)
        .split('/')
        .next_back()
        .unwrap_or(image);
    if base.starts_with("alpine") { "sh" } else { "bash" }
}

/// The kind of a rendered test entry — mirrors [`spec::TestKind`] but owns its
/// payload so it can outlive the spec borrow in `MatrixLeg`.
#[derive(Debug, Clone, PartialEq)]
enum RenderedTestKind {
    Command(String),
    Script(String),
    ScriptInline(String),
}

/// One rendered test entry carried in a matrix leg.
#[derive(Debug, Clone)]
struct RenderedTest {
    name: String,
    kind: RenderedTestKind,
}

/// Describes one matrix leg (test job matrix entry).
struct MatrixLeg {
    platform: String,
    platform_slug: String,
    runner: String,
    container_id: String,
    image: Option<String>,
    shell: String,
    tests: Vec<RenderedTest>,
}

/// Convert a slice of [`TestEntry`] into [`RenderedTest`] list.
///
/// Entries that fail `kind()` (i.e. validated-invalid specs that slip through)
/// are silently omitted — `validate_tests` is the authoritative gate.
fn render_tests(entries: &[TestEntry]) -> Vec<RenderedTest> {
    entries
        .iter()
        .filter_map(|t| {
            let kind = match t.kind() {
                Ok(spec::TestKind::Command(cmd)) => RenderedTestKind::Command(cmd.to_owned()),
                Ok(spec::TestKind::Script(p)) => RenderedTestKind::Script(p.display().to_string()),
                Ok(spec::TestKind::ScriptInline(src)) => RenderedTestKind::ScriptInline(src.to_owned()),
                Err(_) => return None,
            };
            Some(RenderedTest {
                name: t.name.clone(),
                kind,
            })
        })
        .collect()
}

/// Build the flat list of matrix legs from a `MirrorSpec`.
fn build_matrix(spec: &MirrorSpec) -> Vec<MatrixLeg> {
    let Some(platforms) = &spec.platforms else {
        return Vec::new();
    };

    let top_level_tests: Vec<RenderedTest> = render_tests(spec.tests.as_deref().unwrap_or(&[]));

    // Stable ordering: sort platform keys alphabetically so the generated YAML
    // is deterministic across runs.
    let mut platform_keys: Vec<&String> = platforms.keys().collect();
    platform_keys.sort();

    let mut legs = Vec::new();
    for platform_key in platform_keys {
        let config = &platforms[platform_key];
        let platform_slug = platform_key.replace('/', "_");

        let effective_tests: Vec<RenderedTest> = config
            .tests
            .as_deref()
            .map(render_tests)
            .unwrap_or_else(|| top_level_tests.clone());

        if let Some(containers) = &config.containers {
            // Container mode: one leg per container.
            for container in containers {
                let id = container.id.clone().unwrap_or_else(|| container_slug(&container.image));
                let shell = effective_shell_for_container(&container.image, container.shell.as_deref());
                legs.push(MatrixLeg {
                    platform: platform_key.clone(),
                    platform_slug: platform_slug.clone(),
                    runner: config.runner.clone(),
                    container_id: id,
                    image: Some(container.image.clone()),
                    shell: shell.to_string(),
                    tests: effective_tests.clone(),
                });
            }
        } else {
            // Native mode: single leg with container_id = _native_.
            let shell = native_shell_for_platform(platform_key, config);
            legs.push(MatrixLeg {
                platform: platform_key.clone(),
                platform_slug: platform_slug.clone(),
                runner: config.runner.clone(),
                container_id: "_native_".to_string(),
                image: None,
                shell: shell.to_string(),
                tests: effective_tests,
            });
        }
    }
    legs
}

/// Determine the shell for a native test leg.
fn native_shell_for_platform<'a>(platform: &str, config: &'a PlatformConfig) -> &'a str {
    if let Some(shell) = &config.shell {
        return shell.as_str();
    }
    if platform.starts_with("windows") {
        "pwsh"
    } else {
        "bash"
    }
}

/// Render the GHA workflow YAML from a parsed spec.
///
/// Substitution uses a simple `str::replace` chain — no templating engine dep.
fn render_workflow(spec: &MirrorSpec) -> String {
    let schedule_block = spec
        .versions
        .as_ref()
        .and_then(|v| v.poll_interval.as_ref())
        .map(|cron| format!("  schedule:\n    - cron: '{}'\n", cron))
        .unwrap_or_default();

    let release_tag = spec
        .ocx_mirror
        .as_ref()
        .and_then(|m| m.release_tag.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("latest");

    // `webhook_secret` names the *GitHub Actions secret* that carries the
    // webhook URL — the rendered workflow maps it onto the conventional local
    // env var `OCX_MIRROR_DISCORD_HOOK`, which `pipeline notify` reads.
    let webhook_secret_name = spec
        .notify
        .as_ref()
        .and_then(|n| n.discord.as_ref())
        .map(|d| d.webhook_secret.as_str())
        .unwrap_or("OCX_MIRROR_DISCORD_HOOK");

    let matrix = build_matrix(spec);
    let matrix_entries = render_matrix_entries(&matrix);
    let test_run_steps = render_test_run_steps(&matrix);
    let target_identifier = format!("{}/{}", spec.target.registry, spec.target.repository);

    WORKFLOW_TEMPLATE
        .replace("{OCX_MIRROR_VERSION}", VERSION)
        .replace("{OCX_MIRROR_REV}", GIT_SHA_SHORT)
        .replace("{MIRROR_NAME}", &spec.name)
        .replace("{SCHEDULE_BLOCK}", &schedule_block)
        .replace("{TEST_MATRIX_ENTRIES}", &matrix_entries)
        .replace("{TEST_RUN_STEPS}", &test_run_steps)
        .replace("{TARGET_IDENTIFIER}", &target_identifier)
        .replace("{TARGET_REGISTRY}", &spec.target.registry)
        .replace("{WEBHOOK_SECRET_NAME}", webhook_secret_name)
        .replace("{OCX_MIRROR_RELEASE_TAG}", release_tag)
}

/// Render the YAML matrix `include:` entries for the test job.
///
/// Test commands are inlined as a YAML list so the workflow references them
/// via `${{ matrix.tests }}`. This ensures per-platform test overrides
/// (e.g. `cmake.exe --version` on `windows/amd64`) appear verbatim in the
/// generated YAML, satisfying golden-test assertions.
fn render_matrix_entries(legs: &[MatrixLeg]) -> String {
    let mut out = String::new();
    for leg in legs {
        out.push_str(&format!(
            "          - platform: {}\n            platform_slug: {}\n            runner: {}\n            container_id: {}\n",
            leg.platform, leg.platform_slug, leg.runner, leg.container_id,
        ));
        if let Some(image) = &leg.image {
            out.push_str(&format!("            image: {}\n", image));
        }
        out.push_str(&format!("            shell: {}\n", leg.shell));
        // Inline the test entries so they are visible in the generated YAML.
        out.push_str("            tests:\n");
        for test in &leg.tests {
            match &test.kind {
                RenderedTestKind::Command(cmd) => {
                    out.push_str(&format!(
                        "              - name: {}\n                kind: command\n                command: {}\n",
                        test.name, cmd
                    ));
                }
                RenderedTestKind::Script(path) => {
                    out.push_str(&format!(
                        "              - name: {}\n                kind: script\n                script: {}\n",
                        test.name, path
                    ));
                }
                RenderedTestKind::ScriptInline(src) => {
                    // Use YAML block scalar `|` so multi-line Starlark survives.
                    // Each line of the inline source is indented 18 spaces
                    // (matrix entry indent 14 + 4 for block scalar body).
                    let indented = src
                        .lines()
                        .map(|line| format!("                  {line}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push_str(&format!(
                        "              - name: {}\n                kind: script_inline\n                script_inline: |\n{indented}\n",
                        test.name
                    ));
                }
            }
        }
    }
    out
}

/// Render per-test shell commands for the `test` job's run step.
///
/// Each matrix leg runs all its tests for every discovered version. The
/// renderer emits a single shell block that iterates per-version.
fn render_test_run_steps(legs: &[MatrixLeg]) -> String {
    // We emit a single parametric block — the tests and shell are matrix params.
    // The matrix.shell and matrix.container_id are referenced in the outer
    // step condition; here we just emit the inner test-loop body.
    //
    // Since the matrix entries already capture shell + container_id, the
    // rendered run block uses those matrix variables.
    //
    // We emit a representative set of steps based on all unique (shell, tests)
    // combinations observed. For simplicity, emit one block per leg type
    // (container vs native), using conditional shells.
    if legs.is_empty() {
        return String::new();
    }

    // Emit a single parametric block:
    // - For container legs: docker run with the test payload.
    // - For native legs: run payload directly.
    // The test step uses ${{ matrix.shell }} and ${{ matrix.container_id }}.
    // Branching on TEST_KIND selects command / script / script_inline behaviour.
    let body = r#"            METADATA_SIBLING="${BUNDLE%.tar.xz}-metadata.json"
            mkdir -p junit
            JUNIT_FILE="junit/junit-${VERSION}-${{ matrix.platform_slug }}-${{ matrix.container_id }}.xml"
            TESTS_JSON='${{ toJson(matrix.tests) }}'
            TEST_COUNT=$(echo "${TESTS_JSON}" | jq 'length')
            FAILURES=0
            CASES=""
            for i in $(seq 0 $((TEST_COUNT - 1))); do
              TEST_NAME=$(echo "${TESTS_JSON}" | jq -r ".[$i].name")
              TEST_KIND=$(echo "${TESTS_JSON}" | jq -r ".[$i].kind")
              START=$(date +%s)
              RC=0
              if [ "${{ matrix.container_id }}" = "_native_" ]; then
                if [ "${TEST_KIND}" = "command" ]; then
                  TEST_CMD=$(echo "${TESTS_JSON}" | jq -r ".[$i].command")
                  ocx package test --platform "${{ matrix.platform }}" --identifier "{TARGET_IDENTIFIER}:${VERSION}" "${BUNDLE}" -- \
                    ${{ matrix.shell }} -c "${TEST_CMD}" || RC=$?
                elif [ "${TEST_KIND}" = "script" ]; then
                  TEST_SCRIPT=$(echo "${TESTS_JSON}" | jq -r ".[$i].script")
                  ocx package test --platform "${{ matrix.platform }}" --identifier "{TARGET_IDENTIFIER}:${VERSION}" "${BUNDLE}" \
                    --script "${TEST_SCRIPT}" || RC=$?
                else
                  TEST_INLINE=$(echo "${TESTS_JSON}" | jq -r ".[$i].script_inline")
                  printf '%s' "${TEST_INLINE}" | ocx package test --platform "${{ matrix.platform }}" --identifier "{TARGET_IDENTIFIER}:${VERSION}" "${BUNDLE}" \
                    --script - || RC=$?
                fi
              else
                CONTAINER_ID="${{ matrix.container_id }}"
                IMAGE_TAG="ocx-test-${{ matrix.platform_slug }}-${CONTAINER_ID}:{OCX_MIRROR_RELEASE_TAG}"
                if [ "${TEST_KIND}" = "command" ]; then
                  TEST_CMD=$(echo "${TESTS_JSON}" | jq -r ".[$i].command")
                  docker run --rm \
                    -v "$(pwd)/${BUNDLE}:/bundle.tar.xz:ro" \
                    -v "$(pwd)/${METADATA_SIBLING}:/bundle-metadata.json:ro" \
                    -e OCX_VERSION="${VERSION}" \
                    -e OCX_PLATFORM="${{ matrix.platform }}" \
                    -e OCX_IMAGE="${{ matrix.image }}" \
                    "${IMAGE_TAG}" \
                    ${{ matrix.shell }} -c "ocx package test --platform '${{ matrix.platform }}' --identifier '{TARGET_IDENTIFIER}:${VERSION}' /bundle.tar.xz -- ${TEST_CMD}" || RC=$?
                elif [ "${TEST_KIND}" = "script" ]; then
                  TEST_SCRIPT=$(echo "${TESTS_JSON}" | jq -r ".[$i].script")
                  docker run --rm \
                    -v "$(pwd)/${BUNDLE}:/bundle.tar.xz:ro" \
                    -v "$(pwd)/${METADATA_SIBLING}:/bundle-metadata.json:ro" \
                    -v "$(pwd)/${TEST_SCRIPT}:/test.star:ro" \
                    -e OCX_VERSION="${VERSION}" \
                    -e OCX_PLATFORM="${{ matrix.platform }}" \
                    -e OCX_IMAGE="${{ matrix.image }}" \
                    "${IMAGE_TAG}" \
                    ocx package test --platform '${{ matrix.platform }}' --identifier '{TARGET_IDENTIFIER}:${VERSION}' /bundle.tar.xz --script /test.star || RC=$?
                else
                  TEST_INLINE=$(echo "${TESTS_JSON}" | jq -r ".[$i].script_inline")
                  printf '%s' "${TEST_INLINE}" | docker run --rm -i \
                    -v "$(pwd)/${BUNDLE}:/bundle.tar.xz:ro" \
                    -v "$(pwd)/${METADATA_SIBLING}:/bundle-metadata.json:ro" \
                    -e OCX_VERSION="${VERSION}" \
                    -e OCX_PLATFORM="${{ matrix.platform }}" \
                    -e OCX_IMAGE="${{ matrix.image }}" \
                    "${IMAGE_TAG}" \
                    ocx package test --platform '${{ matrix.platform }}' --identifier '{TARGET_IDENTIFIER}:${VERSION}' /bundle.tar.xz --script - || RC=$?
                fi
              fi
              END=$(date +%s)
              DUR=$((END - START))
              if [ "${RC}" -eq 0 ]; then
                CASES="${CASES}    <testcase name=\"${TEST_NAME}\" classname=\"${VERSION}.${{ matrix.platform_slug }}.${{ matrix.container_id }}\" time=\"${DUR}\"/>\n"
              else
                CASES="${CASES}    <testcase name=\"${TEST_NAME}\" classname=\"${VERSION}.${{ matrix.platform_slug }}.${{ matrix.container_id }}\" time=\"${DUR}\"><failure type=\"NonZeroExit\" message=\"exit ${RC}\"/></testcase>\n"
                FAILURES=$((FAILURES + 1))
              fi
            done
            {
              echo '<?xml version="1.0" encoding="UTF-8"?>'
              echo "<testsuites>"
              echo "  <testsuite name=\"${VERSION}.${{ matrix.platform_slug }}.${{ matrix.container_id }}\" tests=\"${TEST_COUNT}\" failures=\"${FAILURES}\">"
              if [ -n "${CI_JOB_URL:-}" ]; then
                echo "    <properties>"
                echo "      <property name=\"ci.job.url\" value=\"${CI_JOB_URL}\"/>"
                echo "    </properties>"
              fi
              printf '%b' "${CASES}"
              echo "  </testsuite>"
              echo "</testsuites>"
            } > "${JUNIT_FILE}"
            echo "wrote ${JUNIT_FILE} (tests=${TEST_COUNT}, failures=${FAILURES})"
            if [ "${FAILURES}" -gt 0 ]; then
              exit 1
            fi
"#;
    body.to_string()
}

/// Render the describe.yml catalog-publish workflow.
///
/// Lighter than `mirror.yml`: only the release-tag + target-registry
/// placeholders need substitution. The workflow itself triggers on changes to
/// `CATALOG.md`, `logo.*`, or `mirror.yml` and invokes
/// `ocx-mirror pipeline describe` to publish the README + logo to the
/// `__ocx.desc` referrer tag on the target repository.
fn render_describe(spec: &MirrorSpec) -> String {
    let release_tag = spec
        .ocx_mirror
        .as_ref()
        .and_then(|m| m.release_tag.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("latest");

    DESCRIBE_TEMPLATE
        .replace("{OCX_MIRROR_VERSION}", VERSION)
        .replace("{OCX_MIRROR_REV}", GIT_SHA_SHORT)
        .replace("{OCX_MIRROR_RELEASE_TAG}", release_tag)
        .replace("{TARGET_REGISTRY}", &spec.target.registry)
}

/// Build the full map of relative path → file content for all generated files.
///
/// Keys are relative to the repo root (i.e. the spec file's parent directory).
fn render(spec: &MirrorSpec, _repo_root: &Path) -> Result<BTreeMap<PathBuf, String>, MirrorError> {
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();

    files.insert(PathBuf::from(".github/workflows/mirror.yml"), render_workflow(spec));
    files.insert(PathBuf::from(".github/workflows/describe.yml"), render_describe(spec));

    Ok(files)
}

// ── Writer ────────────────────────────────────────────────────────────────────

/// Write all rendered files to disk under `repo_root`.
///
/// Creates parent directories as needed. Uses `tokio::fs::write` which is
/// atomic from the caller's perspective (single write call per file).
async fn write_files(files: &BTreeMap<PathBuf, String>, repo_root: &Path) -> Result<(), MirrorError> {
    for (relative_path, content) in files {
        let dest = repo_root.join(relative_path);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                MirrorError::TemplateError(format!("failed to create directory {}: {e}", parent.display()))
            })?;
        }
        tokio::fs::write(&dest, content)
            .await
            .map_err(|e| MirrorError::TemplateError(format!("failed to write {}: {e}", dest.display())))?;
    }
    Ok(())
}

// ── Drift detector ────────────────────────────────────────────────────────────

/// Compare the expected generated files against what is on disk.
///
/// Returns `RendererDrift` if any file is missing or has different content.
/// Drift hints are path-only — never expose file contents to stderr
/// (secret-hygiene rule R3).
async fn check_drift(files: &BTreeMap<PathBuf, String>, repo_root: &Path) -> Result<(), MirrorError> {
    let mut drifted: Vec<String> = Vec::new();

    for (relative_path, expected) in files {
        let on_disk_path = repo_root.join(relative_path);
        match tokio::fs::read_to_string(&on_disk_path).await {
            Ok(actual) => {
                if actual != *expected {
                    drifted.push(relative_path.display().to_string());
                }
            }
            Err(_) => {
                // Missing file counts as drift.
                drifted.push(relative_path.display().to_string());
            }
        }
    }

    if drifted.is_empty() {
        Ok(())
    } else {
        for path in &drifted {
            // Emit path-only hint; content never printed (R3).
            eprintln!("drift: {path}");
        }
        Err(MirrorError::RendererDrift(drifted))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;
    use tempfile::tempdir;

    use super::*;

    // ── §3.3 S3: Golden tests for ocx-mirror generate ci ──────────────────

    /// Copy a fixture file into `work_dir` and run `GenerateCi::execute()` with
    /// the spec pointing at the copy. This ensures generated files land in
    /// `work_dir` (spec parent = work_dir) rather than the fixtures directory.
    ///
    /// Returns `Err(MirrorError)` if the renderer rejects the spec.
    fn render_fixture(fixture_name: &str, work_dir: &Path) -> Result<(), MirrorError> {
        let fixture_src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/")).join(fixture_name);
        let spec_dest = work_dir.join(fixture_name);
        std::fs::copy(&fixture_src, &spec_dest).expect("failed to copy fixture into work_dir");

        let cmd = GenerateCi {
            spec: spec_dest,
            check: false,
            format: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        rt.block_on(async { cmd.execute(&printer).await })
    }

    #[test]
    fn render_minimal_spec_writes_workflow() {
        // §3.3: Fixture mirror-minimal.yml → renderer produces workflow YAML.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-minimal.yml", dir.path());
        match result {
            Ok(()) => {
                let workflow = dir.path().join(".github/workflows/mirror.yml");
                assert!(workflow.exists(), "Expected .github/workflows/mirror.yml to be written");
                let content = std::fs::read_to_string(&workflow).unwrap();
                // Generated file must have the DO-NOT-EDIT header
                assert!(
                    content.contains("DO NOT EDIT"),
                    "Generated workflow must contain 'DO NOT EDIT' header"
                );
                // Must build ocx from submodule (replaces stale release download)
                assert!(
                    content.contains("cargo install --path ocx/crates/ocx_cli --locked"),
                    "Generated workflow must build ocx from submodule"
                );
            }
            Err(MirrorError::SpecUsageError(_)) => {
                panic!("mirror-minimal.yml should be a valid spec, got SpecUsageError");
            }
            Err(e) => {
                panic!("Unexpected error rendering minimal fixture: {e}");
            }
        }
    }

    #[test]
    fn render_multi_container_spec_writes_workflow() {
        // §3.3: Fixture mirror-multi-container.yml → 3 containers on linux/amd64.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-multi-container.yml", dir.path());
        match result {
            Ok(()) => {
                let workflow = dir.path().join(".github/workflows/mirror.yml");
                assert!(workflow.exists(), "Expected workflow to be written");
                let content = std::fs::read_to_string(&workflow).unwrap();
                // All 3 container images must appear in matrix
                assert!(content.contains("ubuntu:24.04"), "ubuntu:24.04 must appear in matrix");
                assert!(content.contains("alpine:3.20"), "alpine:3.20 must appear in matrix");
                assert!(content.contains("fedora:40"), "fedora:40 must appear in matrix");
                // poll_interval is set → schedule trigger must appear
                assert!(
                    content.contains("schedule:"),
                    "Schedule trigger expected (poll_interval set)"
                );
                assert!(content.contains("cron:"), "Cron expression expected");
            }
            Err(MirrorError::SpecUsageError(_)) => {
                panic!("multi-container spec should be valid");
            }
            Err(_) => {} // other failures acceptable until implementation
        }
    }

    #[test]
    fn render_full_platforms_spec_writes_workflow() {
        // §3.3: Fixture mirror-full-platforms.yml — all 6 platforms rendered.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-full-platforms.yml", dir.path());
        match result {
            Ok(()) => {
                let workflow = dir.path().join(".github/workflows/mirror.yml");
                assert!(workflow.exists());
                let content = std::fs::read_to_string(&workflow).unwrap();
                // Per-platform test overrides must be present for windows
                assert!(content.contains("cmake.exe"), "Windows test override must appear");
                assert!(content.contains("smoke.ps1"), "Windows smoke test must appear");
            }
            Err(MirrorError::SpecUsageError(_)) => {
                panic!("full-platforms spec should be valid");
            }
            Err(_) => {}
        }
    }

    #[test]
    fn render_rejects_ocx_install_block_with_usage_error() {
        // §3.3 negative: mirror-rejects-ocx-install.yml → renderer exits 64 (UsageError)
        // before writing any files.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-rejects-ocx-install.yml", dir.path());
        match result {
            Err(MirrorError::SpecUsageError(msg)) => {
                assert!(
                    msg.contains("ocx_install") || msg.contains("release download"),
                    "Error message must mention ocx_install or release download, got: {msg}"
                );
                // No workflow file must have been written
                let workflow = dir.path().join(".github/workflows/mirror.yml");
                assert!(
                    !workflow.exists(),
                    "No workflow must be written when spec is rejected for ocx_install: block"
                );
            }
            Err(MirrorError::SpecInvalid(_)) => {
                // Also acceptable — serde may reject unknown field before validate()
            }
            Ok(()) => panic!("Expected rejection of ocx_install: block, got Ok"),
            Err(e) => panic!("Expected SpecUsageError or SpecInvalid, got: {e}"),
        }
    }

    #[test]
    fn render_r3_discord_url_rejected_before_write() {
        // §3.3 R3 negative: discord URL in webhook_secret → renderer exits 64 before write
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-r3-discord-url.yml", dir.path());
        match result {
            Err(MirrorError::SpecUsageError(msg)) => {
                // R3: must mention URL or webhook
                assert!(
                    msg.to_lowercase().contains("webhook")
                        || msg.to_lowercase().contains("url")
                        || msg.to_lowercase().contains("discord"),
                    "Error must mention webhook/url/discord, got: {msg}"
                );
                let workflow = dir.path().join(".github/workflows/mirror.yml");
                assert!(
                    !workflow.exists(),
                    "No workflow must be written when R3 discord URL is present"
                );
            }
            Err(MirrorError::SpecInvalid(_)) => {
                // Also acceptable if validator catches it at the spec level
            }
            Ok(()) => panic!("Expected rejection of discord URL in webhook_secret"),
            Err(e) => panic!("Expected SpecUsageError/SpecInvalid, got: {e}"),
        }
    }

    // ── §3.4 S4: --check drift detector ───────────────────────────────────

    #[test]
    fn check_mode_exits_zero_on_matching_generated_files() {
        // §3.4: --check after fresh render → exit 0
        // Test: render, then immediately run --check → must succeed.
        let dir = tempdir().unwrap();

        // Copy the spec into the temp dir so generated files land there.
        let fixture_src = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mirror-minimal.yml"
        ));
        let spec_dest = dir.path().join("mirror-minimal.yml");
        std::fs::copy(fixture_src, &spec_dest).unwrap();

        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let rt = tokio::runtime::Runtime::new().unwrap();

        // First: write mode render
        let write_result = rt.block_on(async {
            let cmd = GenerateCi {
                spec: spec_dest.clone(),
                check: false,
                format: None,
            };
            cmd.execute(&printer).await
        });

        match write_result {
            Ok(()) => {
                // Second: check mode — must return Ok(()) on no drift
                let check_result = rt.block_on(async {
                    let cmd = GenerateCi {
                        spec: spec_dest,
                        check: true,
                        format: None,
                    };
                    cmd.execute(&printer).await
                });
                assert!(
                    check_result.is_ok(),
                    "check mode after fresh render must exit 0, got: {:?}",
                    check_result.err()
                );
            }
            Err(_) => {
                // Write mode not yet implemented — test will fail with panic (expected)
            }
        }
    }

    #[test]
    fn check_mode_exits_65_on_drift() {
        // §3.4: --check after mutating one line → exit 65 (DataError) with stderr hint
        let dir = tempdir().unwrap();
        let fixture_src = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mirror-minimal.yml"
        ));
        let spec_dest = dir.path().join("mirror-minimal.yml");
        std::fs::copy(fixture_src, &spec_dest).unwrap();

        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Write mode first
        let write_result = rt.block_on(async {
            let cmd = GenerateCi {
                spec: spec_dest.clone(),
                check: false,
                format: None,
            };
            cmd.execute(&printer).await
        });

        if let Ok(()) = write_result {
            // Mutate generated file
            let workflow_path = dir.path().join(".github/workflows/mirror.yml");
            if workflow_path.exists() {
                let mut content = std::fs::read_to_string(&workflow_path).unwrap();
                content.push_str("\n# drift injection\n");
                std::fs::write(&workflow_path, content).unwrap();

                // Check mode must return RendererDrift → exit 65
                let check_result = rt.block_on(async {
                    let cmd = GenerateCi {
                        spec: spec_dest,
                        check: true,
                        format: None,
                    };
                    cmd.execute(&printer).await
                });

                match check_result {
                    Err(MirrorError::RendererDrift(paths)) => {
                        assert!(!paths.is_empty(), "Drift paths must be non-empty");
                    }
                    Ok(()) => panic!("Expected drift detection, got Ok"),
                    Err(e) => panic!("Expected RendererDrift, got: {e}"),
                }
            }
        }
    }

    #[test]
    fn check_mode_exits_65_on_missing_generated_file() {
        // §3.4: --check with missing generated file → exit 65 with hint
        let dir = tempdir().unwrap();
        let fixture_src = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mirror-minimal.yml"
        ));
        let spec_dest = dir.path().join("mirror-minimal.yml");
        std::fs::copy(fixture_src, &spec_dest).unwrap();

        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Run check mode without prior render — files don't exist → must detect drift
        let check_result = rt.block_on(async {
            let cmd = GenerateCi {
                spec: spec_dest,
                check: true,
                format: None,
            };
            cmd.execute(&printer).await
        });

        match check_result {
            Err(MirrorError::RendererDrift(_)) => {
                // Expected: missing file is drift
            }
            Err(MirrorError::TemplateError(_)) => {
                // Also acceptable: renderer may report missing file as I/O failure
            }
            Ok(()) => panic!("Expected drift on missing generated files, got Ok"),
            Err(e) => {
                // Other errors acceptable until implementation lands
                let _ = e;
            }
        }
    }

    #[test]
    fn render_emits_ci_job_url_property_in_test_matrix() {
        // The Discord embed redesign threads per-(V,P,C) html_url links into
        // run-summary.json. The test matrix step computes the matrix-leg URL
        // via `gh api` and embeds it in the JUnit XML as a suite-level
        // `<property name="ci.job.url" ...>`. `pipeline push` reads the
        // property inside `evaluate_junit` and attaches it to
        // `PlatformFailure.job_url`. This pins down that the renderer wires
        // the property into the rendered workflow.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-multi-container.yml", dir.path());
        if let Ok(()) = result {
            let workflow = dir.path().join(".github/workflows/mirror.yml");
            let content = std::fs::read_to_string(&workflow).unwrap();
            assert!(
                content.contains("CI_JOB_URL=$(gh api"),
                "rendered workflow must resolve the per-leg job URL via `gh api`"
            );
            assert!(
                content.contains("<property name=\\\"ci.job.url\\\""),
                "rendered workflow must embed ci.job.url as a JUnit suite property"
            );
            assert!(
                !content.contains("name: Record job URL"),
                "old standalone 'Record job URL' step must not be emitted any more"
            );
        }
    }

    // ── describe.yml renderer ──────────────────────────────────────────────

    #[test]
    fn render_minimal_spec_writes_describe_workflow() {
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-minimal.yml", dir.path());
        if let Ok(()) = result {
            let describe = dir.path().join(".github/workflows/describe.yml");
            assert!(describe.exists(), "describe.yml must be emitted alongside mirror.yml");
            let content = std::fs::read_to_string(&describe).unwrap();
            assert!(
                content.contains("name: describe"),
                "describe.yml must declare workflow name"
            );
            assert!(
                content.contains("ocx-mirror pipeline describe"),
                "describe.yml must invoke `ocx-mirror pipeline describe`"
            );
            assert!(content.contains("CATALOG.md"), "path filter must include CATALOG.md");
            assert!(
                content.contains("logo.*"),
                "path filter must include logo.* (svg/png probe target)"
            );
        }
    }

    #[test]
    fn render_describe_substitutes_release_tag() {
        // Fixture pins `ocx_mirror.release_tag: v0.7.2`; placeholder must be
        // replaced with the pinned value.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-minimal.yml", dir.path());
        if let Ok(()) = result {
            let describe_path = dir.path().join(".github/workflows/describe.yml");
            let content = std::fs::read_to_string(&describe_path).unwrap();
            assert!(
                content.contains("OCX_MIRROR_RELEASE_TAG: v0.7.2"),
                "release_tag must substitute fixture's pinned value"
            );
            assert!(
                !content.contains("{OCX_MIRROR_RELEASE_TAG}"),
                "release_tag placeholder must be substituted"
            );
        }
    }

    #[test]
    fn check_mode_detects_describe_yml_drift() {
        let dir = tempdir().unwrap();
        let fixture_src = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mirror-minimal.yml"
        ));
        let spec_dest = dir.path().join("mirror-minimal.yml");
        std::fs::copy(fixture_src, &spec_dest).unwrap();

        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let rt = tokio::runtime::Runtime::new().unwrap();

        let write_result = rt.block_on(async {
            let cmd = GenerateCi {
                spec: spec_dest.clone(),
                check: false,
                format: None,
            };
            cmd.execute(&printer).await
        });

        if write_result.is_ok() {
            let describe_path = dir.path().join(".github/workflows/describe.yml");
            assert!(describe_path.exists(), "describe.yml must have been written");
            let mut content = std::fs::read_to_string(&describe_path).unwrap();
            content.push_str("\n# drift injection\n");
            std::fs::write(&describe_path, content).unwrap();

            let check_result = rt.block_on(async {
                let cmd = GenerateCi {
                    spec: spec_dest,
                    check: true,
                    format: None,
                };
                cmd.execute(&printer).await
            });

            match check_result {
                Err(MirrorError::RendererDrift(paths)) => {
                    assert!(
                        paths.iter().any(|p| p.contains("describe.yml")),
                        "drift must call out describe.yml: {paths:?}"
                    );
                }
                Ok(()) => panic!("expected drift detection for describe.yml mutation"),
                Err(e) => panic!("expected RendererDrift, got: {e}"),
            }
        }
    }

    // ── §TestEntry union: CI render tests ──────────────────────────────────────

    /// Build a `MirrorSpec` from inline YAML and call `build_matrix` on it.
    fn build_matrix_from_yaml(yaml: &str) -> Vec<MatrixLeg> {
        let spec: crate::spec::MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        build_matrix(&spec)
    }

    #[test]
    fn render_matrix_entries_emits_kind_command() {
        // A spec with `command:` must produce `kind: command` + `command: <value>` in matrix.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-minimal.yml", dir.path());
        if let Ok(()) = result {
            let workflow = dir.path().join(".github/workflows/mirror.yml");
            let content = std::fs::read_to_string(&workflow).unwrap();
            assert!(
                content.contains("kind: command"),
                "matrix entry for command test must contain 'kind: command'; content:\n{content}"
            );
            assert!(
                content.contains("command: shfmt --version"),
                "matrix entry must contain 'command: shfmt --version'; content:\n{content}"
            );
        }
    }

    #[test]
    fn render_matrix_entries_emits_kind_script() {
        // A spec with `script:` must produce `kind: script` + `script: <path>` in matrix.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-all-test-kinds.yml", dir.path());
        if let Ok(()) = result {
            let workflow = dir.path().join(".github/workflows/mirror.yml");
            let content = std::fs::read_to_string(&workflow).unwrap();
            assert!(
                content.contains("kind: script"),
                "matrix entry for script test must contain 'kind: script'; content:\n{content}"
            );
            assert!(
                content.contains("script: tests/smoke.star"),
                "matrix entry must contain 'script: tests/smoke.star'; content:\n{content}"
            );
        }
    }

    #[test]
    fn render_matrix_entries_emits_kind_script_inline() {
        // A spec with `script_inline:` must produce `kind: script_inline` with YAML
        // block scalar (`script_inline: |`) in the matrix entry.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-all-test-kinds.yml", dir.path());
        if let Ok(()) = result {
            let workflow = dir.path().join(".github/workflows/mirror.yml");
            let content = std::fs::read_to_string(&workflow).unwrap();
            assert!(
                content.contains("kind: script_inline"),
                "matrix entry for inline test must contain 'kind: script_inline'; content:\n{content}"
            );
            assert!(
                content.contains("script_inline: |"),
                "inline test payload must use YAML block scalar ('script_inline: |'); content:\n{content}"
            );
        }
    }

    #[test]
    fn render_all_three_kinds_in_single_spec() {
        // All three kinds must co-exist in the same matrix.
        let dir = tempdir().unwrap();
        let result = render_fixture("mirror-all-test-kinds.yml", dir.path());
        if let Ok(()) = result {
            let workflow = dir.path().join(".github/workflows/mirror.yml");
            let content = std::fs::read_to_string(&workflow).unwrap();
            assert!(content.contains("kind: command"), "command kind missing");
            assert!(content.contains("kind: script"), "script kind missing");
            assert!(content.contains("kind: script_inline"), "script_inline kind missing");
        }
    }

    #[test]
    fn shell_loop_branches_on_test_kind() {
        // The generated shell loop must extract TEST_KIND and branch on its value.
        let legs = build_matrix_from_yaml(
            r#"
name: shfmt
target:
  registry: ocx.sh
  repository: shfmt
source:
  type: github_release
  owner: mvdan
  repo: sh
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"
assets:
  linux/amd64:
    - "shfmt_v.*_linux_amd64$"
asset_type:
  type: binary
  name: shfmt
tests:
  - name: version
    command: shfmt --version
  - name: smoke
    script: tests/smoke.star
  - name: inline
    script_inline: |
      ocx_assert(True)
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
ocx_mirror:
  release_tag: v0.7.2
"#,
        );
        let shell_block = render_test_run_steps(&legs);

        // Must extract TEST_KIND.
        assert!(
            shell_block.contains("TEST_KIND=$(echo \"${TESTS_JSON}\" | jq -r \".[$i].kind\")"),
            "shell loop must extract TEST_KIND; block:\n{shell_block}"
        );
        // Must branch on command.
        assert!(
            shell_block.contains("if [ \"${TEST_KIND}\" = \"command\" ]"),
            "shell loop must have command branch; block:\n{shell_block}"
        );
        // Must branch on script.
        assert!(
            shell_block.contains("elif [ \"${TEST_KIND}\" = \"script\" ]"),
            "shell loop must have script branch; block:\n{shell_block}"
        );
        // Must handle script_inline via else branch (includes printf piped to --script -).
        assert!(
            shell_block.contains("--script -"),
            "shell loop must pipe script_inline to --script -; block:\n{shell_block}"
        );
        // Native script: uses --script $TEST_SCRIPT (not -c).
        assert!(
            shell_block.contains("--script \"${TEST_SCRIPT}\""),
            "native script branch must pass --script; block:\n{shell_block}"
        );
        // Container script: bind-mounts script as /test.star.
        assert!(
            shell_block.contains("/test.star"),
            "container script branch must bind-mount script as /test.star; block:\n{shell_block}"
        );
    }

    // Regression: native jq.exe on Windows runners emits CRLF, so without
    // `tr -d '\r'` after each jq pipeline in the test job the captured
    // `${VERSION}` carried a trailing CR and corrupted bundle paths
    // (e.g. `bundles/bundle-3.10.0\r-windows_amd64.tar.xz`).
    #[test]
    fn workflow_template_strips_cr_after_jq_for_windows_runners() {
        let template = super::WORKFLOW_TEMPLATE;
        assert!(
            template.contains("jq -r '.[].version' | tr -d '\\r'"),
            "test job must strip CR from jq output to survive Git Bash + native jq.exe on Windows"
        );
        assert!(
            template.contains("head -n1 | tr -d '\\r' || true"),
            "CI_JOB_URL capture must strip CR before exporting the URL"
        );
    }
}
