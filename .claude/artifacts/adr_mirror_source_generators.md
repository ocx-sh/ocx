# ADR: Generator-Based URL Index for ocx-mirror

## Metadata

**Status:** Proposed
**Date:** 2026-03-14
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** infrastructure | devops
**Supersedes:** N/A
**Superseded By:** N/A

## Context

The ocx-mirror tool currently supports two source types: `github_release` (paginated GitHub Releases API) and `url_index` (static JSON with version→assets mapping). All four existing mirrors (cmake, bun, git-cliff, go-task) use `github_release`.

Many important tools do **not** publish releases as GitHub Release assets. They use diverse distribution mechanisms:

| Tool | Release Source | Format |
|------|---------------|--------|
| **Node.js** | `nodejs.org/dist/index.json` | JSON API (custom schema — array of version objects with a `files` list, not direct asset URLs) |
| **Amazon Corretto** | GitHub Releases (body text) + `corretto.aws` | Download URLs embedded in release body markdown; actual binaries on `corretto.aws/downloads/resources/{ver}/` |
| **JFrog CLI** | Artifactory directory listing | HTML directory pages on `releases.jfrog.io`; bare binaries (no archives) |
| **Amazon Finch** | GitHub Releases | Standard GitHub Releases (could use `github_release` directly) |
| **zigbuild** | GitHub Releases | Standard GitHub Releases with cargo-dist (could use `github_release` directly) |

The `url_index` source already defines the right interchange format — a flat `{ versions: { ver: { assets: { name: url } } } }` JSON object. The challenge is **generating** that JSON from diverse upstream sources. Each tool's release API has a unique schema that requires custom transformation logic.

### Design Constraints

1. **No new source type.** The `url_index` type already represents the correct abstraction. Extending it with a `generator` field keeps the type system lean.
2. **Strong typing via JSON Schema.** The Rust types (`RemoteIndex`, `RemoteVersionEntry`) are the source of truth. The `ocx_schema` binary should emit a JSON Schema for this format. Python generators should use types generated from that schema — not hand-written dictionaries.
3. **Single-step workflow.** `ocx-mirror sync` should run the generator and consume its output in one step.

## Decision Drivers

- **Diversity of upstream formats**: No single adapter can handle JSON APIs, HTML scraping, and markdown parsing. Each source has bespoke logic.
- **Type safety across language boundary**: Rust types → JSON Schema → generated Python types. No hand-maintained type definitions that can drift.
- **Iteration speed**: Scraping/transformation logic changes frequently. Python is better suited than Rust for this.
- **Reusability**: Common patterns (HTTP fetch, platform name normalization, index construction) should be shared across generators.
- **Tech stack alignment**: Python is already in the project (pytest, uv). `ocx_schema` already generates JSON Schema from Rust types.

## Considered Options

### Option 1: More Rust Source Adapters

**Description:** Add `Source::Nodejs { ... }`, `Source::Corretto { ... }`, etc. as new enum variants in `spec/source.rs`, each with a dedicated Rust adapter module.

| Pros | Cons |
|------|------|
| Type-safe, compiled | Requires recompilation for each new source |
| Single binary, no runtime deps | Rust is verbose for HTTP+JSON scraping |
| Consistent error handling | Each adapter bloats `ocx_mirror` permanently |
| | HTML parsing needs new deps (scraper, select.rs) |
| | Slow iteration cycle for scraping logic tweaks |

### Option 2: New `Source::Script` Type

**Description:** Add a third source variant `Script { command }` that runs an external command and reads `url_index` JSON from stdout.

| Pros | Cons |
|------|------|
| Clean separation | Unnecessary new type — `url_index` already represents this data |
| Explicit in the type system | Three source types when two suffice |
| | `url_index` with `generator` achieves the same with less complexity |

### Option 3: Extend `url_index` with `generator` Field (Chosen)

**Description:** Add an optional `generator` field to `Source::UrlIndex`. When present, the generator command runs first and its stdout is parsed as `url_index` JSON. The existing `url` and `versions` fields remain. Exactly one of the three must be provided.

```yaml
source:
  type: url_index
  generator: ["python", "../../mirror-sdk-py/scripts/nodejs.py"]
```

| Pros | Cons |
|------|------|
| No new source type — extends existing abstraction | `url_index` does three things (but they're the same data shape) |
| Single-step workflow | Requires Python runtime on mirror host |
| Generator output is validated against the same JSON Schema | |
| Minimal Rust change (~30 lines) | |
| Python generators get type safety from generated schema types | |

### Option 4: Declarative URL Template System

**Description:** Config-driven approach with JSONPath expressions and URL templates.

| Pros | Cons |
|------|------|
| No code per source | Too rigid — can't handle HTML scraping or markdown parsing |
| Pure YAML | Complex expressions become unreadable |
| | Can't handle conditional logic (LTS filtering, etc.) |

## Decision Outcome

**Chosen Option:** Option 3 — Extend `url_index` with `generator`

**Rationale:**

The `url_index` source already defines the right data shape. Adding a `generator` field is a natural extension: instead of "fetch this URL" or "use this inline YAML", it's "run this command to produce the JSON." All three modes produce the same `Vec<VersionInfo>` — no new abstractions needed.

Combined with JSON Schema generation from the Rust types and Python type generation from that schema, this gives us end-to-end type safety across the Rust→Python boundary without any hand-maintained type definitions.

### Consequences

**Positive:**
- Adding a new tool mirror = a ~30-line Python script + mirror YAML spec
- Generators validate against the same schema as static `url_index` JSON
- No Rust recompilation for new generators or scraping logic changes
- The `ocx_schema` extension is reusable for any future cross-language contract

**Negative:**
- Python runtime required on mirror host (already present for tests; all CI runners have it)
- Two languages in the pipeline — but the boundary is clean (JSON Schema contract)

**Risks:**
- Script execution security: Mitigated — mirror specs are authored by project maintainers, not external users. Scripts run with the same privileges as `ocx-mirror` itself.
- Schema drift: Mitigated — Python types are generated from the JSON Schema, which is generated from Rust types. The chain is automated.

## Technical Details

### Architecture

```
                    Source of Truth
                         │
              ┌──────────┴──────────┐
              │   Rust types in     │
              │   ocx_mirror        │
              │   (RemoteIndex,     │
              │    RemoteVersionEntry)
              └──────────┬──────────┘
                         │
                    ocx_schema
                         │
              ┌──────────┴──────────┐
              │   JSON Schema       │
              │   url_index/v1.json │
              └──────────┬──────────┘
                         │
                 datamodel-codegen
                         │
              ┌──────────┴──────────┐
              │   Python types      │
              │   (generated)       │
              │   ocx_gen/schema.py │
              └──────────┬──────────┘
                         │
              ┌──────────┴──────────┐
              │   Generator scripts │
              │   (nodejs.py, etc.) │
              │   use typed builder │
              └──────────┬──────────┘
                         │
                    stdout (JSON)
                         │
              ┌──────────┴──────────┐
              │   ocx-mirror sync   │
              │   url_index adapter │
              │   (existing pipeline│
              │    unchanged)       │
              └─────────────────────┘
```

### 1. Rust Changes: `url_index` Generator Field

**`spec/source.rs`** — Model mutual exclusion as an enum:

```rust
/// Configuration for an external generator command.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct GeneratorConfig {
    /// Command to execute. Must output url_index JSON to stdout.
    /// First element is the executable, rest are arguments.
    pub command: Vec<String>,
    /// Working directory for the command.
    /// Relative paths are resolved from the mirror spec directory.
    /// Default: the spec directory.
    pub working_directory: Option<String>,
}

/// The three modes of providing url_index data.
///
/// Exactly one mode must be used. This is enforced at both the schema level
/// (`oneOf`) and the deserialization level (`#[serde(untagged)]`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub enum UrlIndexSource {
    /// Fetch url_index JSON from a remote URL.
    Remote {
        url: String,
    },
    /// Inline version→assets map directly in the mirror spec.
    Inline {
        versions: HashMap<String, UrlIndexVersion>,
    },
    /// Run an external command that outputs url_index JSON to stdout.
    Generator {
        generator: GeneratorConfig,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub enum Source {
    GithubRelease {
        owner: String,
        repo: String,
        #[serde(default = "default_tag_pattern")]
        tag_pattern: String,
    },
    UrlIndex(UrlIndexSource),
}

impl Source {
    pub fn validate(&self, errors: &mut Vec<String>) {
        match self {
            Source::GithubRelease { tag_pattern, .. } => {
                // ... existing tag_pattern regex validation ...
            }
            Source::UrlIndex(UrlIndexSource::Generator { generator }) => {
                if generator.command.is_empty() {
                    errors.push("source.generator.command must be a non-empty list".to_string());
                }
            }
            _ => {}
        }
    }
}
```

**What this achieves:**

- **Schema-level enforcement**: `#[serde(untagged)]` on `UrlIndexSource` produces a JSON Schema `oneOf` with three subschemas. Schema validators reject objects that match zero or multiple variants.
- **Deserialization-level enforcement**: serde tries each variant in order and fails if none match. No runtime `validate()` check needed for mutual exclusion.
- **No more `Option` juggling**: the old three-`Option` pattern with a manual count check is replaced by a type that makes illegal states unrepresentable.

**YAML remains unchanged** — `#[serde(untagged)]` means there's no discriminant field. The existing YAML shapes all work:

```yaml
# Remote URL
source:
  type: url_index
  url: "https://example.com/versions.json"

# Inline versions
source:
  type: url_index
  versions:
    "1.0.0":
      assets:
        tool.tar.gz: "https://example.com/tool.tar.gz"

# Generator command
source:
  type: url_index
  generator:
    command: ["uv", "run", "generate.py"]
```

#### Working Directory Resolution

The generator's working directory determines where the command runs. This matters because tools like `uv` and `bun` discover their project config (`pyproject.toml`, `package.json`) by searching upward from the working directory.

**Default: the spec directory.** If `working_directory` is set, it is resolved relative to the spec directory.

This keeps the common case trivial — colocated scripts like `generate.py` sit next to `mirror.yml`, so the default cwd is already the right place:

```yaml
# Colocated script — cwd is mirrors/nodejs/ (spec directory, default)
# uv reads PEP 723 inline metadata from generate.py → installs deps
generator:
  command: ["uv", "run", "generate.py"]

# Script in a subdirectory — explicit override
generator:
  command: ["bun", "run", "src/generate.ts"]
  working_directory: src
```

**`source/url_index.rs`** — Add `from_generator()`:

```rust
use crate::spec::GeneratorConfig;

/// Run a generator command and parse its stdout as url_index JSON.
pub async fn from_generator(config: &GeneratorConfig, spec_dir: &Path) -> anyhow::Result<Vec<VersionInfo>> {
    let working_dir = config.resolve_working_directory(spec_dir);

    let output = tokio::process::Command::new(&config.command[0])
        .args(&config.command[1..])
        .current_dir(&working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run generator '{}': {e}", config.command[0]))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "generator '{}' failed (exit {}): {}",
            config.command.join(" "),
            output.status,
            stderr.trim()
        );
    }

    if output.stdout.is_empty() {
        anyhow::bail!("generator '{}' produced no output", config.command.join(" "));
    }

    let index: RemoteIndex = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("generator output is not valid url_index JSON: {e}"))?;

    parse_remote_index(index)
}
```

Working directory resolution lives on `GeneratorConfig`:

```rust
impl GeneratorConfig {
    /// Resolve the working directory for this generator.
    /// Default: spec directory. If `working_directory` is set, resolve relative to spec dir.
    pub fn resolve_working_directory(&self, spec_dir: &Path) -> PathBuf {
        match &self.working_directory {
            Some(wd) => spec_dir.join(wd),
            None => spec_dir.to_path_buf(),
        }
    }
}
```

The version listing dispatch matches exhaustively — no `unreachable!()`:

```rust
Source::UrlIndex(source) => match source {
    UrlIndexSource::Remote { url } => url_index::from_remote(url).await?,
    UrlIndexSource::Inline { versions } => url_index::from_inline(versions)?,
    UrlIndexSource::Generator { generator } => {
        url_index::from_generator(generator, spec_dir).await?
    }
},
```

### 2. JSON Schema Generation

**Move `RemoteIndex` types to a shared location with `JsonSchema` derives.**

The `RemoteIndex` and `RemoteVersionEntry` types currently live in `source/url_index.rs` as private structs. To generate a JSON Schema from them:

**Option A**: Move them to `ocx_lib` behind a `mirror` feature or module (since they're a general interchange format).

**Option B**: Keep them in `ocx_mirror` and have `ocx_schema` depend on `ocx_mirror` with a `jsonschema` feature flag.

**Recommended: Option B** — these types are specific to the mirror interchange format. Adding a `jsonschema` feature to `ocx_mirror` mirrors the existing pattern in `ocx_lib`.

**`crates/ocx_mirror/Cargo.toml`**:
```toml
[features]
jsonschema = ["dep:schemars"]

[dependencies]
schemars = { version = "1", optional = true }
```

**`source/url_index.rs`** — Make types public, add derives:

```rust
/// Root of the url_index JSON format.
///
/// Contains a map of version strings to their release assets.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct RemoteIndex {
    /// Map of version string (e.g., "22.15.0") to version entry.
    pub versions: HashMap<String, RemoteVersionEntry>,
}

/// A single version's metadata and download assets.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct RemoteVersionEntry {
    /// Whether this is a pre-release version.
    #[serde(default)]
    pub prerelease: bool,
    /// Map of asset filename to download URL.
    pub assets: HashMap<String, String>,
}
```

**`crates/ocx_schema/Cargo.toml`**:
```toml
[dependencies]
ocx_lib = { path = "../ocx_lib", features = ["jsonschema"] }
ocx_mirror = { path = "../ocx_mirror", features = ["jsonschema"] }
```

**`crates/ocx_schema/src/main.rs`** — Extend to emit multiple schemas:

```rust
use ocx_lib::package::metadata::Metadata;
use ocx_mirror::source::url_index::RemoteIndex;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = args.get(1).map(|s| s.as_str()).unwrap_or("metadata");

    match target {
        "metadata" => generate_schema::<Metadata>("https://ocx.sh/schemas/metadata/v1.json"),
        "url-index" => generate_schema::<RemoteIndex>("https://ocx.sh/schemas/url-index/v1.json"),
        other => {
            eprintln!("unknown schema target: {other}");
            eprintln!("available: metadata, url-index");
            std::process::exit(1);
        }
    }
}

fn generate_schema<T: schemars::JsonSchema>(id: &str) {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());
    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();

    let mut value = serde_json::to_value(&schema).expect("failed to serialize schema");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("$id".to_owned(), serde_json::Value::String(id.to_owned()));
    }

    let json = serde_json::to_string_pretty(&value).expect("failed to serialize schema");
    println!("{json}");
}
```

**Taskfile** — Add schema generation target:

```yaml
schema:generate-url-index:
  cmds:
    - mkdir -p {{.SCHEMA_DIR}}/url-index
    - cargo run -p ocx_schema --release -- url-index > {{.SCHEMA_DIR}}/url-index/v1.json
```

### 3. Generator Framework

Generators can be written in any language — the contract is "exit 0 + url_index JSON on stdout." The shared libraries provide convenience, not enforcement.

#### Principle: Generator Scripts Live in the Mirror Directory

Each mirror owns its generator script. The shared `ocx_gen` library is an external dependency consumed via PEP 723 inline script metadata — not by colocating scripts inside the library project.

```
mirror-sdk-py/                             # Shared library only (no scripts)
├── pyproject.toml                      # Defines ocx-gen as installable package
├── src/ocx_gen/
│   ├── __init__.py                     # Re-exports IndexBuilder
│   ├── _schema.py                      # GENERATED from JSON Schema (do not edit)
│   ├── index.py                        # IndexBuilder — typed wrapper
│   ├── http.py                         # Retry-aware HTTP client (httpx)
│   └── platforms.py                    # Platform name normalization
└── tests/
    └── test_index.py                   # Library unit tests

mirrors/
├── nodejs/
│   ├── mirror.yml                      # Mirror spec
│   ├── generate.py                     # Generator script (declares its own deps)
│   └── metadata.json
├── corretto/
│   ├── mirror.yml
│   ├── generate.py
│   └── metadata.json
└── jfrog-cli/
    ├── mirror.yml
    ├── generate.py
    └── metadata.json
```

#### How Scripts Reference the Shared Library: PEP 723 Inline Script Metadata

Each generator script declares its dependencies — including a path reference to `ocx_gen` — directly in the file using [PEP 723 inline script metadata][pep-723]. uv reads this block and creates an isolated environment with all deps installed.

```python
# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen", "httpx"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///

from ocx_gen import IndexBuilder
from ocx_gen.http import fetch_json

def main():
    index = IndexBuilder()
    # ... fetch upstream releases, build index ...
    index.emit()

if __name__ == "__main__":
    main()
```

When `uv run mirrors/nodejs/generate.py` executes:
1. uv reads the inline `# /// script` metadata block
2. Creates an isolated virtual environment for this script
3. Installs `ocx-gen` from the path dependency (`../../mirror-sdk-py`) and `httpx` from PyPI
4. Runs the script with all dependencies available

**Benefits of this approach:**
- **No workspace, no per-mirror `pyproject.toml`** — each script is self-describing
- **Each mirror can have different additional deps** — e.g., `beautifulsoup4` for HTML scraping in one mirror, not polluting others
- **Scripts are independently runnable** — `uv run mirrors/nodejs/generate.py` works from anywhere
- **The mirror directory is the unit of ownership** — script, spec, metadata, all together

#### Shared Library: `mirror-sdk-py/pyproject.toml`

```toml
[project]
name = "ocx-gen"
version = "0.1.0"
requires-python = ">=3.13"
dependencies = ["httpx>=0.28"]

[project.optional-dependencies]
dev = ["datamodel-code-generator>=0.26", "pytest>=8", "ruff>=0.9"]

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
```

The library has no scripts — it provides `IndexBuilder`, `fetch_json`, `fetch_text`, and the generated schema types. Generator scripts are consumers, not part of the package.

#### Multi-Runtime Support (Bun, TypeScript)

For TypeScript generators, a parallel shared library exists and mirror scripts reference it directly:

```
mirror-sdk-ts/                          # Shared TS library
├── package.json
├── src/ocx-gen/
│   ├── schema.ts                       # GENERATED from JSON Schema (quicktype)
│   ├── index.ts                        # IndexBuilder
│   └── http.ts                         # HTTP client
```

A TypeScript mirror generator imports the library via relative path:

```ts
// mirrors/some-tool/generate.ts
import { IndexBuilder } from "../../mirror-sdk-py-ts/src/ocx-gen/index.js";
import { fetchJson } from "../../mirror-sdk-py-ts/src/ocx-gen/http.js";
// ... generator logic ...
```

```yaml
# mirrors/some-tool/mirror.yml
source:
  type: url_index
  generator:
    command: ["bun", "run", "generate.ts"]
    # cwd inferred: mirrors/some-tool/ (script directory)
```

The JSON Schema is the shared contract — `datamodel-code-generator` produces Python types, `quicktype --lang typescript` produces TS types, both from the same schema.

[pep-723]: https://peps.python.org/pep-0723/

#### Schema → Python Type Generation

Use [`datamodel-code-generator`][datamodel-codegen] to generate Python dataclasses from the JSON Schema:

```bash
# In CI or as a task:
cargo run -p ocx_schema --release -- url-index > /tmp/url-index-schema.json
datamodel-codegen \
    --input /tmp/url-index-schema.json \
    --output mirror-sdk-py/src/ocx_gen/_schema.py \
    --output-model-type dataclasses.dataclass \
    --target-python-version 3.13
```

This produces typed Python dataclasses that match the Rust types exactly:

```python
# mirror-sdk-py/src/ocx_gen/_schema.py  (GENERATED — do not edit)
from __future__ import annotations
from dataclasses import dataclass, field

@dataclass
class RemoteVersionEntry:
    """A single version's metadata and download assets."""
    assets: dict[str, str]
    """Map of asset filename to download URL."""
    prerelease: bool = False
    """Whether this is a pre-release version."""

@dataclass
class RemoteIndex:
    """Root of the url_index JSON format."""
    versions: dict[str, RemoteVersionEntry]
    """Map of version string to version entry."""
```

#### `ocx_gen.index` — Typed Builder

The `IndexBuilder` wraps the generated types with a convenient builder API and JSON emission:

```python
"""Typed builder for url_index JSON, backed by schema-generated types."""

import json
import sys
from ocx_gen._schema import RemoteIndex, RemoteVersionEntry


class IndexBuilder:
    """Build a url_index JSON document with type-safe version entries."""

    def __init__(self) -> None:
        self._versions: dict[str, RemoteVersionEntry] = {}

    def add_version(
        self,
        version: str,
        *,
        assets: dict[str, str],
        prerelease: bool = False,
    ) -> None:
        """Add a version with its assets.

        Args:
            version: Semver version string (e.g., "22.15.0").
            assets: Map of asset filename to download URL.
            prerelease: Whether this is a pre-release.
        """
        if not assets:
            return  # Skip versions with no assets
        self._versions[version] = RemoteVersionEntry(
            assets=assets,
            prerelease=prerelease,
        )

    def build(self) -> RemoteIndex:
        """Return the constructed RemoteIndex."""
        return RemoteIndex(versions=self._versions)

    def emit(self, file=sys.stdout) -> None:
        """Serialize to JSON and write to the given file (default: stdout)."""
        index = self.build()
        # dataclasses → dict conversion
        data = {
            "versions": {
                ver: {"prerelease": entry.prerelease, "assets": entry.assets}
                for ver, entry in index.versions.items()
            }
        }
        json.dump(data, file, indent=2)
        file.write("\n")
```

#### `ocx_gen.http` — HTTP Client

```python
"""Retry-aware HTTP client for generator scripts."""

import httpx

_CLIENT: httpx.Client | None = None

def _get_client() -> httpx.Client:
    global _CLIENT
    if _CLIENT is None:
        _CLIENT = httpx.Client(
            timeout=30.0,
            follow_redirects=True,
            transport=httpx.HTTPTransport(retries=3),
        )
    return _CLIENT

def fetch_json(url: str) -> object:
    """Fetch a URL and parse the response as JSON."""
    response = _get_client().get(url)
    response.raise_for_status()
    return response.json()

def fetch_text(url: str) -> str:
    """Fetch a URL and return the response body as text."""
    response = _get_client().get(url)
    response.raise_for_status()
    return response.text
```

### 4. Example Generator: Node.js

```python
# /// script
# requires-python = ">=3.13"
# dependencies = ["ocx-gen", "httpx"]
#
# [tool.uv.sources]
# ocx-gen = { path = "../../mirror-sdk-py" }
# ///
"""Generate url_index JSON for Node.js releases."""

from ocx_gen import IndexBuilder
from ocx_gen.http import fetch_json

DIST_URL = "https://nodejs.org/dist"
INDEX_URL = f"{DIST_URL}/index.json"

# Platform identifiers in Node's "files" array → archive extension
PLATFORMS = {
    "linux-x64": ".tar.xz",
    "linux-arm64": ".tar.xz",
    "darwin-x64": ".tar.gz",
    "darwin-arm64": ".tar.gz",
    "win-x64": ".zip",
}

def main():
    releases = fetch_json(INDEX_URL)
    index = IndexBuilder()

    for release in releases:
        version = release["version"].lstrip("v")
        files = set(release.get("files", []))

        assets: dict[str, str] = {}
        for platform, ext in PLATFORMS.items():
            if platform in files:
                filename = f"node-{release['version']}-{platform}{ext}"
                assets[filename] = f"{DIST_URL}/{release['version']}/{filename}"

        if assets:
            # Node.js: lts is False for non-LTS, or a codename string for LTS
            is_prerelease = release.get("lts") is False
            index.add_version(version, assets=assets, prerelease=is_prerelease)

    index.emit()

if __name__ == "__main__":
    main()
```

### 5. Example Mirror Spec: Node.js

```yaml
# mirrors/nodejs/mirror.yml
name: nodejs
target:
  registry: ocx.sh
  repository: nodejs

source:
  type: url_index
  generator:
    command: ["uv", "run", "generate.py"]
    # cwd inferred: mirrors/nodejs/ (script directory)
    # uv reads PEP 723 metadata from generate.py → installs ocx-gen + httpx

assets:
  linux/amd64:
    - "node-.*-linux-x64\\.tar\\.xz"
  linux/arm64:
    - "node-.*-linux-arm64\\.tar\\.xz"
  darwin/amd64:
    - "node-.*-darwin-x64\\.tar\\.gz"
  darwin/arm64:
    - "node-.*-darwin-arm64\\.tar\\.gz"
  windows/amd64:
    - "node-.*-win-x64\\.zip"

strip_components: 1
cascade: true
build_timestamp: none
skip_prereleases: true

metadata:
  default: metadata.json

versions:
  min: "20.0.0"
  new_per_run: 5

concurrency:
  max_downloads: 4
  max_bundles: 2
  max_pushes: 2
  rate_limit_ms: 200
  max_retries: 3
```

### 6. Error Handling Contract

| Script Behavior | Mirror Response |
|-----------------|-----------------|
| Exit 0 + valid JSON on stdout | Parse and proceed |
| Exit 0 + invalid JSON on stdout | Fail with deserialization error + schema hint |
| Exit non-zero | Fail with exit code + stderr content |
| Timeout (configurable, default 60s) | Kill process, fail with timeout error |
| Stdout empty | Fail with "no output" error |

### 7. Type Safety Chain

```
Rust types (source of truth)
    │
    ▼  schemars + ocx_schema
JSON Schema (url-index/v1.json)
    │
    ▼  datamodel-code-generator
Python dataclasses (_schema.py)
    │
    ▼  IndexBuilder wrapper
Generator scripts (nodejs.py, etc.)
    │
    ▼  stdout JSON
ocx-mirror (deserializes with same Rust types)
```

At no point in this chain are types hand-maintained in two places. If the Rust type changes:
1. `ocx_schema` produces an updated JSON Schema
2. `datamodel-codegen` produces updated Python types
3. Generator scripts get type errors if they produce incompatible data
4. `ocx-mirror` deserializes with the updated Rust type

### 8. Taskfile Integration

```yaml
# taskfiles/mirror-sdk.taskfile.yml

schema:generate-url-index:
  desc: Generate JSON Schema for url_index format
  cmds:
    - mkdir -p {{.SCHEMA_DIR}}/url-index
    - cargo run -p ocx_schema --release -- url-index > {{.SCHEMA_DIR}}/url-index/v1.json

mirror-sdk:codegen:
  desc: Generate Python types from url_index JSON Schema
  deps: [schema:generate-url-index]
  dir: mirror-sdk-py
  cmds:
    - >
      uv run datamodel-codegen
      --input {{.SCHEMA_DIR}}/url-index/v1.json
      --output src/ocx_gen/_schema.py
      --output-model-type dataclasses.dataclass
      --target-python-version 3.13

mirror-sdk:test:
  desc: Run generator tests
  dir: mirror-sdk-py
  cmds:
    - uv run pytest tests/
```

## Implementation Plan

1. [ ] **Rust: Add `generator` field to `Source::UrlIndex`** — Update `spec/source.rs` validation (exactly one of url/versions/generator). Add `from_generator()` in `source/url_index.rs`.
2. [ ] **Rust: Make `RemoteIndex` types public with JsonSchema** — Add `schemars` feature to `ocx_mirror`, derive `JsonSchema` on `RemoteIndex`/`RemoteVersionEntry`. Add `Serialize` derive.
3. [ ] **Rust: Extend `ocx_schema`** — Add `url-index` target, depend on `ocx_mirror` with `jsonschema` feature.
4. [ ] **Python: Create `mirror-sdk-py/` project** — `pyproject.toml` with uv, `datamodel-code-generator` as dev dep, `httpx` as runtime dep.
5. [ ] **Python: Generate `_schema.py`** — Run `datamodel-codegen` from the JSON Schema.
6. [ ] **Python: Implement `ocx_gen` library** — `index.py` (IndexBuilder), `http.py`, `platforms.py`.
7. [ ] **Mirror: Create `mirrors/nodejs/`** — `generate.py` (with PEP 723 inline metadata), `mirror.yml`, `metadata.json`.
8. [ ] **Test: End-to-end** — Verify `ocx-mirror sync mirrors/nodejs/mirror.yml` works.
9. [ ] **Mirrors: Additional generators** — `mirrors/corretto/generate.py`, `mirrors/jfrog-cli/generate.py`.
10. [ ] **Taskfile: Wire up codegen and test targets.**

## Validation

- [ ] `ocx_schema -- url-index` produces valid JSON Schema
- [ ] Generated Python types match the Rust types
- [ ] `IndexBuilder.emit()` output deserializes correctly in Rust
- [ ] `ocx-mirror sync` with a generator source works end-to-end
- [ ] Generator failures surface clear error messages
- [ ] Schema regeneration + Python codegen can be triggered with a single `task` command

## Links

- [ocx-mirror ADR](./adr_ocx_mirror.md)
- [datamodel-code-generator](https://github.com/koxudaxi/datamodel-code-generator)
- [Node.js dist index](https://nodejs.org/dist/index.json)
- [Amazon Corretto downloads](https://docs.aws.amazon.com/corretto/latest/corretto-21-ug/downloads-list.html)
- [JFrog CLI releases](https://releases.jfrog.io/artifactory/jfrog-cli/v2-jf/)

[datamodel-codegen]: https://github.com/koxudaxi/datamodel-code-generator

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-14 | mherwig + Claude | Initial draft |
| 2026-03-14 | mherwig + Claude | Revised: extend url_index instead of new type; add JSON Schema → Python codegen chain |
