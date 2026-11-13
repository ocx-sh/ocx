# Research: Fulcio certificate claim OIDs for a generic `claims` match

Date: 2026-11-13 · Spike for `plan_publisher_trust_client.md` (Fulcio claim names and value encoding, checked against Fulcio's own OID document before any name is fixed).

## Sources

Read 2026-11-13 against Fulcio `main`:

- OID document: <https://raw.githubusercontent.com/sigstore/fulcio/main/docs/oid-info.md>
- Provider mapping: <https://raw.githubusercontent.com/sigstore/fulcio/main/config/identity/config.yaml>
- GitHub principal: <https://raw.githubusercontent.com/sigstore/fulcio/main/pkg/identity/github/principal.go>
- GitLab principal: <https://raw.githubusercontent.com/sigstore/fulcio/main/pkg/identity/gitlabcom/principal.go>

## Naming

Fulcio spells each extension in Title Case in the OID document ("Source Repository Identifier"), kebab-case in `config.yaml` (`source-repository-identifier`) and CamelCase in Go. The claim key is the document name lowercased with `_` for spaces; the issuer V2 extension is `issuer`.

## OID table

All OIDs sit under the arc `1.3.6.1.4.1.57264.1.`. "+" means Fulcio prepends `https://github.com/`.

| .N | Fulcio name | Claim key | Encoding | GitHub source | GitLab source |
|---|---|---|---|---|---|
| 1 | Issuer (deprecated) | OID only | raw | `iss` | `iss` |
| 2 | GitHub Workflow Trigger (deprecated) | OID only | raw | `event_name` | none |
| 3 | GitHub Workflow SHA (deprecated) | OID only | raw | `sha` | none |
| 4 | GitHub Workflow Name (deprecated) | OID only | raw | `workflow` | none |
| 5 | GitHub Workflow Repository (deprecated) | OID only | raw | `repository` | none |
| 6 | GitHub Workflow Ref (deprecated) | OID only | raw | `ref` | none |
| 7 | OtherName SAN | not a claim | inside the SAN extension | none | none |
| 8 | Issuer (V2) | `issuer` | DER UTF8String | `iss` | `iss` |
| 9 | Build Signer URI | `build_signer_uri` | DER | + `job_workflow_ref` | `https://` + `ci_config_ref_uri` |
| 10 | Build Signer Digest | `build_signer_digest` | DER | `job_workflow_sha` | `ci_config_sha` |
| 11 | Runner Environment | `runner_environment` | DER | `runner_environment` | `runner_environment` |
| 12 | Source Repository URI | `source_repository_uri` | DER | + `repository` | `https://gitlab.com/` + `project_path` |
| 13 | Source Repository Digest | `source_repository_digest` | DER | `sha` | `sha` |
| 14 | Source Repository Ref | `source_repository_ref` | DER | `ref` | `refs/heads/` or `refs/tags/` + `ref` |
| 15 | Source Repository Identifier | `source_repository_identifier` | DER | `repository_id` | `project_id` |
| 16 | Source Repository Owner URI | `source_repository_owner_uri` | DER | + `repository_owner` | base + `namespace_path` |
| 17 | Source Repository Owner Identifier | `source_repository_owner_identifier` | DER | `repository_owner_id` | `namespace_id` |
| 18 | Build Config URI | `build_config_uri` | DER | + `workflow_ref` | `https://` + `ci_config_ref_uri` |
| 19 | Build Config Digest | `build_config_digest` | DER | `workflow_sha` | `ci_config_sha` |
| 20 | Build Trigger | `build_trigger` | DER | `event_name` | `pipeline_source` |
| 21 | Run Invocation URI | `run_invocation_uri` | DER | + `repository`/actions/runs/`run_id`/attempts/`run_attempt` | base/`project_path`/-/jobs/`job_id` |
| 22 | Source Repository Visibility At Signing | `source_repository_visibility_at_signing` | DER | `repository_visibility` | `project_visibility` |
| 23 | Deployment Environment | `deployment_environment` | DER | `environment` (`config.yaml` only) | `environment` (`config.yaml` only) |
| 24 | Token Subject | `token_subject` | DER | raw `sub` | raw `sub` |

Unverified: which principal types fill `.1.23` and `.1.24` (not in the GitHub `principal.go` read). Both stay in the name table as names Fulcio documents; a test against a real certificate confirms them before any guide recommends them.

## Decoding rule

- `.1.1`–`.1.6`: the extension value is the string itself. Compare the raw bytes as UTF-8; never DER-parse.
- `.1.8` and up: the value is a DER UTF8String (tag `0x0C`) with no trailing bytes; compare its content. A malformed value is a non-match, with no fallback to raw.
- `.1.7` lives inside the SAN extension and is not matchable as a claim.

## Decisions this supports

1. Claim keys are the snake_case of Fulcio's document names; `.1.8` is `issuer`.
2. The name table maps `.1.8`–`.1.24` (excluding `.1.7`) only. Legacy `.1.1`–`.1.6` are reachable by raw OID key and compared raw.
3. A raw OID key is accepted only under the `1.3.6.1.4.1.57264.1.` arc; any other dotted key matches nobody.
4. Decoding dispatches on N: raw for N ≤ 6, strict DER UTF8String for N ≥ 8. An OID key with N above the table (a future Fulcio field) decodes as DER, the shape every V2 field has.
5. Comparison is byte-exact string equality; every listed claim must match.
6. An absent extension never matches, not even `""`; a present, empty extension matches only `""`.
7. `oidc_issuer` keeps comparing against `.1.8` (today's `identity.rs`); the `.1.1` fallback is not added in this slice.
8. Guides prefer `source_repository_identifier` and `source_repository_owner_identifier` (numeric IDs survive a rename or transfer) over the URI forms.
9. `build_signer_uri` is the called (reusable) workflow, `build_config_uri` the top-level caller; they differ under a reusable workflow, which is the own-release case (`oci-publish.yml` called from `release.yml`).

## Pitfalls

- Email and username certificates carry only the issuer extensions, none of `.1.9`–`.1.23`; the local Sigstore stack (`test/sigstore/fulcio-config.json`, type `email`) mints only those.
- GitLab `ci_config_ref_uri` has no scheme, so Fulcio prepends `https://`; `source_repository_ref` is synthesised from `ref` and `ref_type`. On GitLab the `build_signer_*` and `build_config_*` values are identical.
- Empty values are legal for `.1.15`, `.1.16`, `.1.17`, `.1.22`, `.1.23`.
- Duplicate-extension handling is not specified by Fulcio; RFC 5280 forbids duplicates, so a certificate carrying one is refused by the parser or treated as non-matching.
