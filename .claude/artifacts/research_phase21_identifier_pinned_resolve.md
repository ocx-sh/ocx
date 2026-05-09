# Research — Phase 2.1 Identifier / PinnedIdentifier / resolve.json

**Scope:** pre-stub research for plan_project_toolchain.md Phase 2.1.
**Date:** 2026-04-20
**Reviewer:** worker-researcher + worker-explorer (parallel).

## Q1 — `oci::Identifier` deserialization

### Three parse entry points

| Entry point | Strict? | Behaviour | File:line |
|---|---|---|---|
| `Identifier::parse(input)` | **Strict** | Calls `validate_segments` then `has_explicit_registry`; returns `IdentifierErrorKind::MissingRegistry` if first segment has no `.`/`:` and is not `"localhost"`. | `identifier.rs:55-64` |
| `Identifier::parse_with_default_registry(s, default)` | Permissive | Calls `parse_internal(s, default)` directly. Bare `"cmake:3.28"` succeeds, gets `default` registry. | `identifier.rs:71-74` |
| `Identifier::from_str` (`FromStr`) | Permissive | Calls `parse_internal(value, DEFAULT_REGISTRY)` directly. Bare `"cmake:3.28"` → `ocx.sh/cmake:3.28`. | `identifier.rs:185-188` |

**Plan §1 contradiction:** plan says "use `Identifier::from_str`" but §1 narrative requires bare-tag rejection. `from_str` is permissive — it silently expands `OCX_DEFAULT_REGISTRY` and that's exactly what F1 says must NOT happen.

**Resolution:** use `Identifier::parse` (strict). The existing `Identifier::Deserialize` impl already uses `parse` (`identifier.rs:205`).

### Error variant for missing registry

- `IdentifierErrorKind::MissingRegistry` — `identifier/error.rs:47`
- Display: `"identifier must include an explicit registry (e.g. 'ocx.sh/tool:1.0', not 'tool:1.0')"`
- Wrapped in `IdentifierError { input, kind }` displayed as `"invalid identifier '{input}': {kind}"` — `identifier/error.rs:9`
- Existing test `deserialize_rejects_bare_name` (`identifier.rs:756-759`) confirms behaviour.

### Display round-trip

`Identifier::Display` (`identifier.rs:170-180`) round-trips for all four canonical forms (`reg/repo`, `reg/repo:tag`, `reg/repo@digest`, `reg/repo:tag@digest`) — confirmed by `display_roundtrip` (`identifier.rs:582-594`) and `display_with_digest_roundtrip` (`identifier.rs:596-603`).

### Visitor-vs-key pattern (cleanest)

The existing `Identifier::Deserialize` impl rejects bare values at the value position but the serde error has no access to the map key. Plan §1 requires `ToolValueMissingRegistry { name, value }` carrying both.

**Recommended pattern:** two-pass via `RawProjectConfig`.

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProjectConfig {
    #[serde(default)]
    tools: BTreeMap<String, String>,
    #[serde(default, rename = "group")]
    groups: BTreeMap<String, BTreeMap<String, String>>,
}

impl ProjectConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, ProjectError> {
        let raw: RawProjectConfig = toml::from_str(s).map_err(...)?;
        validate(raw)  // walks maps, calls Identifier::parse per value, maps MissingRegistry → ToolValueMissingRegistry { name, value }
    }
}
```

This gives post-validate access to both map key and value. A custom `Visitor` at the value position cannot access the key.

## Q2 — `PinnedIdentifier::strip_advisory` and `eq_content`

### Semantics

- `strip_advisory()` returns `Self(self.0.without_tag())` — new `PinnedIdentifier` with registry+repo+digest preserved, tag dropped. Returns `PinnedIdentifier`, not `Identifier` — digest invariant preserved. (`pinned_identifier.rs:41-43`, `identifier.rs:111-118`)
- `eq_content()` compares registry + repository + digest only; advisory tag ignored. (`pinned_identifier.rs:31-35`)
- Derived `PartialEq`/`Eq` includes ALL fields including tag, so `with_tag != without_tag` even with same digest. (`pinned_identifier.rs:232-243`)

### Serialization

- `Serialize` impl calls `self.to_string()` → `Display` → `{registry}/{repo}[:{tag}][@{digest}]`. (`pinned_identifier.rs:77-84`, `71-75`)
- **Tags ARE preserved on serialize.** No auto-strip. Test `deserialize_preserves_tag` confirms (`pinned_identifier.rs:267-273`).
- `Deserialize` impl uses strict `Identifier::parse` then `PinnedIdentifier::try_from` (requires digest). (`pinned_identifier.rs:86-95`)

### Existing call sites — strip_advisory used as DEDUP KEY ONLY

| File:line | Function | Use |
|---|---|---|
| `package/resolved_package.rs:70` | `with_dependencies` | strips for transitive-dep dedup `HashMap<PinnedIdentifier, usize>` key |
| `package/resolved_package.rs:85` | `with_dependencies` | same for direct dep |
| `package_manager/tasks/pull.rs:189` | `setup_package` | singleflight dedup key |

**All three sites use stripped value as a hash key only.** Stored `PinnedIdentifier` in `ResolvedDependency.identifier` retains the original tag (`resolved_package.rs:77`, `91`).

## Q3 — `resolve.json` write-path policy

### Locations

`PackageStore::resolve(identifier) -> PathBuf` at `file_structure/package_store.rs:147` — the canonical sidecar path: `<package_dir>/resolve.json`.

### Writer

`pull.rs:464` writes via `resolved.write_json(pkg.resolve())`. The `resolved` is a `ResolvedPackage` containing `Vec<ResolvedDependency>` where `identifier: PinnedIdentifier` is the **original, unstripped** identifier (constructed at `resolved_package.rs:77,91`). `PinnedIdentifier::Serialize` writes the full Display form including the advisory tag.

### Reader

`package_manager/tasks/common.rs:92` (loaded via `SerdeExt::read_json` → `serde_json::from_reader`). Uses strict `Identifier::parse` + `PinnedIdentifier::try_from`. Accepts both tagged and untagged forms.

### Asymmetry assessment

**No round-trip asymmetry.** Both reader and writer accept both forms.

**Intentional policy divergence with `ocx.lock`** (per plan §7.4):
- `resolve.json` (install-time sidecar, ~/.ocx/packages/...): preserves tag for human readability
- `ocx.lock` (project root, committed): strips advisory tag — Phase 2.1 step 4

Step 9 produces no behavioural code change to `resolve.json`. The work is documentation-only: a comment at `pull.rs:464` (writer) explaining the divergence so future readers don't try to "harmonise" the two policies.

## Implementation guidance for stub builder

1. **Use `Identifier::parse` (strict), NOT `Identifier::from_str`.** The plan's "from_str" reference is wrong; `parse` matches the plan §1 narrative.
2. **Two-pass `from_toml_str`:** `RawProjectConfig` (with `BTreeMap<String, String>`) → `validate(raw)` walks maps, calls `Identifier::parse` per value, maps `IdentifierErrorKind::MissingRegistry` → `ProjectErrorKind::ToolValueMissingRegistry { name, value }`.
3. **For other `IdentifierError` kinds** (invalid chars, malformed digest, etc.) — add a sibling variant `ProjectErrorKind::ToolValueInvalid { name, value, source: IdentifierError }`. The plan only names `ToolValueMissingRegistry` but other errors still come from `parse`; either subsume them under `ToolValueInvalid` or risk losing diagnostic context.
4. **`PinnedIdentifier::strip_advisory()` returns `PinnedIdentifier`** (not `Identifier`) — preserves digest, drops tag. Step 4 calls this when constructing `LockedTool.pinned`.
5. **`PinnedIdentifier` serializes with tag** if present. The lock writer must explicitly strip before constructing `LockedTool.pinned`.
6. **`resolve.json` does NOT strip.** Step 9 is documentation-only (no behavioural change). Add an inline comment at `pull.rs:464` noting the divergence with `ocx.lock`.
7. **`eq_content()` is the comparator for `generated_at` preservation** (plan §7.1). Two `Vec<LockedTool>` are "content-equal" iff each `pinned` pair satisfies `eq_content` — tag-only changes never bust preservation.
8. **`Identifier::Display` round-trips.** Hash input (plan §4) uses `Display` form — safe; no normalisation surprises.
9. **`PinnedIdentifier::Deserialize` is strict** (`Identifier::parse` then `try_from`). Lock-reader accepts only fully-qualified strings — always satisfied since lock writer always emits fully-qualified.
10. **Stub shape:** `RawProjectConfig` (private, `pub(super)`) + `ProjectConfig` (public). `from_toml_str(s)` is the only entry; `from_path(p)` reads file then delegates.
