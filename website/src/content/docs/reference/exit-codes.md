---
title: "Exit codes"
---
<!-- doc_type: reference -->

Codes follow BSD sysexits.h where one fits. Codes 79 to 82 are specific to OCX.

Values are stable across releases, so branching on `$?` is supported. Exit numbers 83, 84, 85, 86, 87 are retired and never reused.

Under `--format json` a failure names its code's kind and its slug in the [error document](./machine-interface.md#machine-interface-error-document).

## `0` Success {#exit-0}

The command succeeded.

## `1` Failure {#exit-1}

The command failed and no more specific code applies.

- Error kind: `internal`

| Slug | Meaning |
| --- | --- |
| `activation_failed` | Shell activation failed with an unclassified cause |
| `announce_failed` | Announcing failed with an unclassified cause |
| `any_pin_provenance_unavailable` | Verifying an `any` pin's provenance failed with an unclassified cause |
| `archive_internal` | An internal archive operation failed |
| `bin_scan_failed` | Scanning for interface binaries failed with an unclassified cause |
| `client_internal` | A registry operation failed with an unclassified cause |
| `compression_engine_init` | The compression engine failed to initialize |
| `dependency_pin_resolution_failed` | Resolving a dependency pin failed with an unclassified cause |
| `dependency_pin_verification_failed` | Verifying a dependency pin failed with an unclassified cause |
| `dependency_routing_failed` | Routing a dependency through the index failed with an unclassified cause |
| `dependency_setup_failed` | Setting up a dependency failed with an unclassified cause |
| `forge_client_build` | The forge HTTP client could not be built |
| `forge_decode` | A forge response could not be decoded |
| `forge_failed` | A forge operation failed with an unclassified cause |
| `forge_missing_field` | A forge response lacks a required field |
| `forge_request_encode` | A forge request body could not be encoded |
| `forge_required` | No forge was supplied to read the committed index root through |
| `forge_status` | The forge answered with an unexpected HTTP status |
| `fork_base_unreachable` | The fork's branch cannot reach the upstream base |
| `fork_field_missing` | A fork response lacks a required field |
| `fork_not_ready` | The fork did not become ready in time |
| `fork_owner_mismatch` | The fork is owned by another account than expected |
| `fork_parent_absent` | The fork reports no parent |
| `fork_parent_mismatch` | The fork's parent is not the expected upstream |
| `git_command_failed` | A git command failed |
| `git_push_failed` | A git push failed for an unrecognised reason |
| `index_path_invalid` | A local index path is invalid |
| `index_root_missing_field` | The committed index root lacks a required field |
| `index_root_not_object` | The committed index root is not a JSON object |
| `index_root_parse` | The committed index root is not valid JSON |
| `index_singleflight_failed` | A shared index operation failed with an unclassified cause |
| `index_source_failed` | An index source failed with an unclassified cause |
| `internal` | A failure no classifier recognises |
| `internal_path_invalid` | An internal store path has an unexpected structure |
| `launch_spawn_failed` | The resolved command could not be started |
| `layer_layout_failed` | Resolving a layer layout failed with an unclassified cause |
| `layer_not_staged` | A layer the package needs was not staged |
| `libc_lint_scan_failed` | Walking the content tree for the libc check failed with an unclassified cause |
| `malformed_fork_full_name` | A fork's full name is malformed |
| `malformed_physical_repository` | The index root names a malformed registry repository |
| `managed_config_update_failed` | Syncing the managed-config snapshot failed with an unclassified cause |
| `missing_base_ref` | The index base branch does not exist |
| `missing_head_root` | A concurrent claim's or announce's head carries no index root |
| `package_internal` | A package operation failed with an unclassified cause |
| `package_task_failed` | A per-package operation failed with an unclassified cause |
| `patch_discovery_failed` | Discovering patches failed with an unclassified cause |
| `project_load_failed` | Loading the project failed with an unclassified cause |
| `project_manifest_edit_diverged` | The edited ocx.toml no longer matches the staged configuration |
| `push_option_refused` | The forge refused a git push option |
| `record_inputs_incomplete` | An execution record frame carries no argv |
| `setup_bootstrap_failed` | Installing ocx into the content store failed with an unclassified cause |
| `singleflight_abandoned` | The leader of shared in-flight work stopped without a result |
| `singleflight_failed` | Shared in-flight work failed with an unclassified cause |
| `task_panicked` | A package task panicked |
| `unknown_compare_status` | A forge compare returned a status the client does not model |
| `unyank_tag_not_curated` | A tag to unyank is not a curated tag |
| `yank_tag_not_curated` | A tag to yank is not a curated tag |
| `yank_unyank_overlap` | The same tag is both yanked and unyanked |

## `64` UsageError {#exit-64}

The command line is invalid: unknown flag, wrong argument count or bad syntax.

- Error kind: `usage_error`

| Slug | Meaning |
| --- | --- |
| `binding_already_exists` | A binding with this name already exists |
| `binding_ambiguous` | A binding exists in several groups and no group was named |
| `binding_not_selected` | The named binding is declared in no selected group |
| `bot_identity` | The acting identity is a bot account |
| `delete_needs_tag` | A tag delete names no single tag, or also names a digest |
| `destination_not_a_directory` | The destination exists and is not a directory |
| `destination_not_empty` | The destination directory is not empty |
| `destination_through_symlink` | A destination path resolves through a symlink |
| `duplicate_binding_across_groups` | A binding is defined in more than one selected group |
| `duplicate_owner` | The same owner was supplied twice |
| `empty_group_filter` | A group filter names an empty group |
| `forge_kind_unknown` | The forge kind of a self-hosted host is unknown; name it explicitly |
| `forge_transport_operation_unsupported` | The selected transport does not support this operation |
| `forge_transport_unsupported` | The forge does not support the selected transport |
| `fork_host_mismatch` | The fork lives on another host than the index |
| `index_named_by_digest` | The source names an image index by digest, which cannot merge into the target |
| `invalid_binding_name` | A binding name carries a character outside the allowed set |
| `invalid_command_line` | The command line names an unknown flag or nested subcommand, or a missing or malformed argument |
| `invalid_endpoint_url` | A Sigstore endpoint URL failed validation |
| `invalid_group_name` | A group name carries a character outside the allowed set |
| `invalid_layer_ref` | A layer reference or its layout suffix does not parse |
| `invalid_owner_login` | An owner login carries a character outside letters, digits, dot, underscore and hyphen |
| `invalid_repo_coordinate` | A repository coordinate is not a valid owner/name |
| `invalid_tag` | A tag is malformed, or not one the operation may act on |
| `invalid_version_spec` | The version argument is not a tag, a digest, or a tag pinned to a digest |
| `key_reference_invalid` | A key reference could not be parsed |
| `malformed_repository` | The repository is not a well-formed oci:// pointer |
| `metadata_ambiguous` | More than one candidate package metadata file was found |
| `metadata_invalid_layer_path` | A layer path cannot anchor a metadata file search |
| `metadata_required` | No package metadata file was given or found |
| `nested_namespace_unsupported` | The forge does not support nested namespaces |
| `no_acting_identity` | No owner could be determined to write into the claim |
| `no_curated_tags` | No tag is left to announce once reserved tags are dropped |
| `no_identity_provided` | No signer identity was given by flags or a matching trust policy |
| `no_matching_platform` | The source index offers no requested platform |
| `no_project` | No ocx.toml exists in the working directory or any parent |
| `no_project_in` | The explicitly selected project directory holds no ocx.toml |
| `ocx_home_needs_no_consent` | The ocx home is always active and takes no consent stamp |
| `owner_id_mismatch` | A supplied owner id disagrees with the forge account |
| `platform_ambiguous` | The source is a single-platform manifest and more than one platform was named |
| `platform_required` | The source is a single-platform manifest and no platform was named |
| `project_already_exists` | ocx.toml already exists at the target path |
| `provenance_version_unsupported` | The provenance predicate type is older than SLSA v1.0 |
| `prune_digest_tag` | A digest was given where prune takes a tag |
| `prune_not_a_prerelease` | The --prerelease value is not a pre-release without a build |
| `prune_package_not_bare` | The package argument carries a tag or digest |
| `rekor_upload_required_for_keyless` | Skipping the transparency log upload requires a signing key |
| `self_fork_refused` | The fork namespace is the upstream's own |
| `sidecar_requires_signature` | A sidecar signature format was requested without a signing identity |
| `terminal_output_refused` | Unsanitized document bytes were bound for a terminal; redirect to a file or a pipe |
| `unknown_group` | A group filter names a group ocx.toml does not declare |
| `unsigned_type_unsupported` | An unsigned attach names a predicate type with no SBOM media type |
| `usage` | The command was invoked with arguments it cannot act on |
| `users_api_unavailable` | The forge offers no users API to resolve owners through |

## `65` DataError {#exit-65}

Input data is malformed or fails verification.

- Error kind: `data_error`

| Slug | Meaning |
| --- | --- |
| `agnostic_platform_libc_claim` | A libc-agnostic platform ships a binary that needs a libc |
| `ambiguous_host_leaf` | ocx.lock pins several platforms this host could run, none preferred |
| `any_pin_not_advertised_as_any` | A dependency pinned as `any` is not published for any platform |
| `archive_entry_escape` | An archive entry path escapes the extraction root |
| `archive_extracted_no_entries` | The archive extracted no entries |
| `archive_extraction_cap_exceeded` | Extraction exceeded the decompressed-size cap |
| `archive_gnu_sparse_unsupported` | An archive entry uses the unsupported GNU sparse format |
| `archive_hard_link_escape` | An archive hard link does not resolve inside the extraction root |
| `archive_symlink_escape` | An archive symlink points outside the extraction root |
| `archive_tar_invalid` | The tar stream is malformed |
| `archive_unsupported_format` | The archive format is not supported |
| `archive_zip_invalid` | The zip archive is malformed |
| `attestation_budget_exhausted` | The attestations together exceed the total size limit |
| `attestation_payload_too_large` | The attestation statement exceeds its size limit |
| `attestation_too_large` | The attestation envelope exceeds its size limit |
| `bin_scan_unsupported_host` | This host cannot scan for the target platform's executables |
| `builder_mismatch` | The provenance builder does not match the pinned builder |
| `bundle_parse_failed` | The signature bundle could not be parsed |
| `candidate_limit_exhausted` | The examination cap was reached with candidates unexamined and none passing |
| `cert_chain_invalid` | The certificate chain does not verify against the trust root |
| `certificate_validity_window` | The log entry time falls outside the certificate validity window |
| `client_serialization` | A registry document could not be serialized or parsed |
| `command_not_executable` | The command resolves to a file that is not executable |
| `command_not_found` | The command does not resolve in the composed environment |
| `command_trampoline_refused` | The command resolves to a toolchain launcher that would re-enter itself |
| `committed_tags_dropped` | The announce would drop tags the index already committed |
| `compression_decode_only` | The format can be extracted but not written |
| `compression_unknown_format` | The compression algorithm could not be determined |
| `credential_helper_invalid_json` | The credential helper answered with invalid JSON |
| `declared_binary_not_executable` | A declared binary is not executable |
| `decompression_cap_exceeded` | A layer decompressed past the size cap |
| `dependency_ambiguous_platform` | Several of a dependency's platforms match the target equally |
| `dependency_conflict` | Dependencies pin conflicting versions of one repository |
| `dependency_declaration_invalid` | A package declares its dependencies invalidly |
| `dependency_no_compatible_platform` | A dependency is published for no platform compatible with the target |
| `dependency_pinned_to_index` | A dependency is pinned to an image index rather than a platform manifest |
| `desc_disappeared` | The package description vanished between two reads |
| `digest_mismatch` | Downloaded content does not hash to its expected digest |
| `digest_missing` | The identifier carries no digest after resolution |
| `direct_digest_pin_in_any_target` | An `any` package pins a dependency by a platform-specific digest |
| `dispatch_object_digest_mismatch` | A stored dispatch object does not hash to its claimed digest |
| `duplicate_platform_key` | ocx.lock lists the same platform twice for one binding |
| `empty_push_set` | A push names no target platform |
| `entrypoint_collision` | Two packages declare the same entrypoint name |
| `extra_ca_certs_invalid` | The extra CA bundle the payload names is not usable |
| `extra_ca_certs_not_utf8` | The extra CA bundle is not UTF-8 and cannot be persisted |
| `extra_ca_file_invalid` | The extra CA bundle file holds no usable certificate |
| `feature_mismatch` | The package needs a platform feature this host lacks |
| `forwarded_env_invalid` | The OCX_ENV value a parent ocx forwarded is malformed |
| `index_dispatch_digest_mismatch` | A dispatch object does not hash to the digest its root claims |
| `index_serialization` | An index document could not be serialized |
| `integration_namespace_invalid` | An integrations namespace is invalid |
| `integration_too_large` | An integrations payload exceeds its size limit |
| `integrations_too_large` | A package's integrations exceed the per-package size limit |
| `invalid_binary_name` | A binary name is not valid |
| `invalid_boolean_string` | A value is not one of the accepted boolean spellings |
| `invalid_build_metadata` | A version's build metadata is invalid |
| `invalid_digest` | A digest is not a valid algorithm-prefixed hash |
| `invalid_encoding` | Registry content is not valid UTF-8 |
| `invalid_identifier` | A package identifier does not parse |
| `invalid_image_index` | An image index document is invalid |
| `invalid_layer_layout` | A layer's strip-components or prefix annotation is invalid |
| `invalid_list_separator` | A package env entry declares an unusable list separator |
| `invalid_logo_content` | The logo file's content does not match its format |
| `invalid_manifest` | A manifest is invalid |
| `invalid_platform` | A platform string does not follow the platform grammar |
| `json_serialization` | JSON could not be serialized or deserialized |
| `key_malformed` | A signing or verification key holds no usable key |
| `launcher_path_not_utf8` | A path baked into a generated launcher is not valid UTF-8 |
| `launcher_unsafe_character` | A value baked into a generated launcher contains an unsafe character |
| `layer_size_exceeded` | A layer declares a size above the allowed maximum |
| `libc_scan_scope_modifier` | The libc scan scope carries a modifier it cannot honour |
| `libc_scan_scope_unresolvable` | The libc scan scope cannot be resolved |
| `link_path_occupied` | The install link path holds something other than an ocx package link |
| `list_separator_invalid` | A list-valued environment entry conflicts with or edges on its separator |
| `lock_stale` | ocx.lock does not match ocx.toml |
| `malformed_catalog_key` | An index source served a malformed catalog key |
| `malformed_index_document` | An index document is not valid JSON of the expected shape |
| `malformed_physical_ref` | An index root carries a malformed physical repository reference |
| `malformed_root_document` | A local index root document is not valid JSON of the expected shape |
| `managed_config_entry_too_large` | The managed-config config.toml entry exceeds the allowed size |
| `managed_config_invalid_archive` | The managed-config layer is not a readable archive |
| `managed_config_invalid_toml` | The fetched managed-config payload is not valid TOML |
| `managed_config_layer_digest_mismatch` | The managed-config layer does not hash to its declared digest |
| `managed_config_layer_size_exceeded` | The managed-config layer declares a size above the allowed maximum |
| `managed_config_missing_config_toml` | The managed-config layer contains no config.toml |
| `managed_config_no_any_platform` | The managed-config package has no any/any platform entry |
| `managed_config_no_gzip_layer` | The managed-config package has no tar+gzip layer |
| `managed_config_unexpected_manifest` | The managed-config package has an unexpected manifest shape |
| `metadata_blob_too_large` | A package metadata blob exceeds the size cap |
| `missing_digest` | An identifier the store needs pinned carries no digest |
| `missing_list_separator` | A list-valued package env entry omits its separator |
| `multiple_attestations` | More than one verified attestation matched |
| `multiple_signatures` | The envelope carries other than exactly one signature |
| `nested_image_index` | An image index nests another image index, which OCI does not support |
| `non_utf8_wire_name` | A local index path is not valid UTF-8 |
| `noncanonical_platform_key` | ocx.lock records a platform key not in canonical form |
| `not_a_manifest` | The registry did not answer with a manifest |
| `package_authoring_invalid` | Package metadata cannot be authored as given |
| `package_manager_serialization` | A package document could not be serialized |
| `package_serialization` | A package document could not be serialized |
| `patch_descriptor_invalid_json` | A patch descriptor is not valid JSON of the expected shape |
| `patch_descriptor_too_large` | A patch descriptor exceeds its structural limits |
| `patch_descriptor_unsupported_version` | A patch descriptor has a version this build does not understand |
| `patch_layer_digest_mismatch` | A patch descriptor layer does not hash to its declared digest |
| `patch_layer_size_exceeded` | A patch descriptor layer declares a size above the allowed maximum |
| `patch_manifest_digest_mismatch` | A patch descriptor manifest does not hash to its declared digest |
| `patch_snapshot_unsupported_version` | The patch snapshot has a format version this build does not understand |
| `patch_unexpected_artifact_type` | A patch descriptor manifest carries an unexpected artifact type |
| `patch_unexpected_layer_media_type` | A patch descriptor layer carries an unexpected media type |
| `patch_unexpected_manifest` | A patch descriptor manifest has an unexpected shape |
| `patch_wrong_layer_count` | A patch descriptor manifest carries other than exactly one layer |
| `path_escape` | A relative path is absolute, escapes its root, or is otherwise unsafe to join |
| `payload_type_unsupported` | The envelope payload type is not in-toto JSON |
| `pin_digest_mismatch` | The registry resolved the pinned tag to a different digest |
| `pinned_identifier_missing_digest` | An identifier that must be pinned carries no digest |
| `predicate_not_json` | The predicate file is not JSON |
| `predicate_too_large` | The predicate or statement exceeds its size limit |
| `predicate_type_mismatch` | The signed predicate type is not the requested one |
| `project_env_path_separator_in_value` | An ocx.toml path value contains the platform path separator |
| `pull_request_unmergeable` | The open announce pull request conflicts with the index |
| `rekor_inclusion_proof_absent` | The bundle carries no transparency log inclusion proof |
| `rekor_set_absent_tsa_present` | The bundle has a timestamp authority stamp but no transparency log timestamp, which this build cannot verify |
| `rekor_set_invalid` | The transparency log timestamp does not verify against the log key |
| `rekor_set_malformed` | The transparency log answered with an unusable timestamp or body |
| `repository_escapes_index_home` | An index repository path escapes its source directory |
| `repository_mismatch` | A re-claim supplied a repository other than the committed one |
| `reserved_env_key` | A package env entry sets a reserved OCX key |
| `root_name_mismatch` | The committed index root names a different package |
| `root_references_unwritten_object` | The announce would commit a root that references an object it does not carry |
| `root_repository_mismatch` | A derived index root points at another repository |
| `sbom_media_type_unsupported` | An unsigned SBOM referrer declares a media type outside the SBOM set |
| `selection_ambiguous` | The selection matches more than one package |
| `separator_edged_list_value` | A package list value starts or ends with its separator |
| `shim_claim_unfulfilled` | A declared interface name does not exist in the package |
| `shim_name_invalid` | A shim name is not a valid binary name |
| `shim_name_not_claimed` | A shim name is not an interface name the package declares |
| `shim_names_not_enumerable` | The package's interface names cannot be enumerated |
| `signature_invalid` | The signature does not verify over the subject digest |
| `simple_signing_claim_unsupported` | The simple signing payload declares an unsupported claim type |
| `statement_subject_absent` | The signed statement carries no subjects |
| `statement_subject_mismatch` | No subject in the signed statement binds the target digest |
| `statement_subject_weak_algorithm` | A statement subject carries no sha256 digest |
| `statement_type_unsupported` | The statement type is not an accepted in-toto statement type |
| `subject_digest_mismatch` | The registry served subject bytes that do not hash to the resolved digest |
| `subject_digest_unsupported` | The subject digest is not sha256 |
| `symlink_requires_tag` | Symlink resolution needs a tag, not a digest |
| `tag_not_an_image_index` | A tag to announce does not point at an image index |
| `template_ambiguous_dependency_ref` | An interpolation token names an ambiguous dependency |
| `template_ambiguous_self_env_ref` | An interpolation token names an env var declared more than once |
| `template_disallowed_token` | An interpolation token is not permitted in this field |
| `template_modifier_not_applicable` | An interpolation modifier does not apply to its token |
| `template_undefined_self_env_ref` | An interpolation token names an env var not declared before it |
| `template_unknown_dependency_ref` | An interpolation token names an unknown dependency |
| `template_unknown_field` | An interpolation token names an unknown field |
| `template_unknown_modifier` | An interpolation token carries an unknown modifier |
| `template_unknown_token` | An interpolation token is not recognised |
| `template_value_too_large` | An interpolated value exceeds its size budget |
| `tlog_binding_mismatch` | The transparency log entry does not match the received envelope |
| `too_many_attestations` | The target has more attestation referrers than the candidate limit |
| `toolchain_home_not_absolute` | The toolchain home is not an absolute path |
| `transparency_body_mismatch` | The transparency log entry belongs to another subject |
| `transparency_log_response_invalid` | The transparency log answered with a public-key body that is oversize or not text |
| `traversal_limit_exceeded` | Copying a manifest graph exceeded a traversal limit |
| `undeclared_binary` | A scanned executable is not declared in binaries |
| `undeclared_libc` | A binary needs a libc the package's platform does not declare |
| `unexpected_artifact_type` | An artifact carries an unexpected artifact type |
| `unexpected_layer_media_type` | A layer carries an unexpected media type |
| `unexpected_manifest_type` | An image index arrived where an image manifest was expected |
| `unfollowed_redirect` | The registry answered with a redirect the transport did not follow |
| `unknown_env_modifier` | A package env entry declares a type this build does not know |
| `unparseable_elf` | A file carries an ELF header but cannot be parsed |
| `unrecognized_interpreter` | A binary names an ELF interpreter no libc family claims |
| `unsafe_destination` | The registry named a destination the transport refuses |
| `unsupported_index_format` | The index format version is not supported |
| `unsupported_logo_format` | The logo file has an unsupported format |
| `unsupported_media_type` | A manifest carries an unsupported media type |
| `unsupported_tlog_entry_kind` | The transparency log entry kind is not accepted |
| `version_invalid` | A package version does not parse |
| `walked_digest_mismatch` | A source answered a digest request with different content |
| `wrong_layer_count` | An artifact carries other than exactly one layer |
| `yanked_refused` | The resolved version is yanked |

## `69` Unavailable {#exit-69}

A required service is unavailable and rerunning will not help.

- Error kind: `unavailable`

| Slug | Meaning |
| --- | --- |
| `catalog_document_absent` | The index source serves no catalog document |
| `endpoint_unresolvable` | An endpoint host does not resolve |
| `forge_transport_failed` | The forge could not be reached |
| `forge_unavailable` | The forge answered with a server error |
| `git_unavailable` | The git executable the transport needs is unavailable |
| `index_http_failed` | An index request failed |
| `registry_unavailable` | A registry operation failed in a way a retry does not clear |
| `registry_unreachable` | The registry could not be reached |
| `setup_profile_subprocess` | A shell subprocess setup needs could not run |
| `transparency_log_key_unavailable` | The transparency log answered its public-key request with a client error |
| `transparency_log_unreachable` | The transparency log cannot be reached and a rerun will not change that |

## `74` IoError {#exit-74}

A filesystem read or write failed.

- Error kind: `io_error`

| Slug | Meaning |
| --- | --- |
| `archive_io` | Reading or writing an archive entry failed |
| `ci_export_write` | Writing the CI export stream failed |
| `ci_file_write` | Writing a CI export file failed |
| `client_io` | Reading or writing a local file during a registry operation failed |
| `compression_create` | Creating compressed output failed |
| `compression_io` | A compression stream failed to read or write |
| `compression_open` | Opening a file for decompression failed |
| `config_edit_io` | Reading or writing the config file being edited failed |
| `config_edit_malformed` | The config file being edited has a shape the edit cannot apply to |
| `config_edit_parse` | The config file being edited does not parse as TOML |
| `config_io` | Reading a config file failed |
| `credential_store_write` | Writing the credential store failed |
| `destination_check_io` | Inspecting the destination directory failed |
| `extra_ca_certs_read_failed` | Reading the extra CA bundle the payload names failed |
| `extra_ca_file_too_large` | The extra CA bundle file exceeds the allowed size |
| `extra_ca_unreadable` | The extra CA bundle cannot be read |
| `file_io` | Reading or writing an internal file failed |
| `key_unreadable` | A signing or verification key could not be read from its location |
| `libc_lint_read` | Reading a file while checking its libc requirement failed |
| `managed_config_payload_read_failed` | Reading the managed-config payload failed |
| `managed_config_snapshot_write` | Writing the managed-config snapshot failed |
| `managed_config_stage_failed` | Staging the managed-config payload for publishing failed |
| `output_write` | Writing the output tree failed |
| `package_file_io` | Reading or writing a package file failed |
| `package_manager_internal_file` | Reading or writing an internal package file failed |
| `patch_blob_write_failed` | Persisting a patch descriptor blob failed |
| `project_internal_file` | Reading or writing an internal project file failed |
| `project_io` | Reading or writing a project file failed |
| `project_registry_io` | Reading or writing the project registry failed |
| `record_exemption_refused` | The execution-record exemption was refused |
| `record_serialize_failed` | Serializing an execution record failed |
| `record_write_failed` | Writing an execution record failed |
| `same_filesystem_check_failed` | Probing whether two paths share a filesystem failed |
| `setup_io` | Reading or writing a setup file failed |
| `symlink_walk_io` | Inspecting a destination path component failed |
| `trust_root_unreadable` | A trust root file named by the operator could not be read |
| `trusted_root_read_failed` | Reading the trusted root the payload names failed |

## `75` TempFail {#exit-75}

A transient failure; the same command may succeed on retry.

- Error kind: `temp_fail`

| Slug | Meaning |
| --- | --- |
| `config_edit_locked` | Another process holds the lock on the config file being edited |
| `credential_helper_timeout` | The credential helper did not answer in time |
| `forge_non_fast_forward` | The push was not a fast-forward; another writer moved the branch |
| `forge_stale_lease` | The branch moved since it was read |
| `forge_transient` | The forge answered with a status a retry may clear |
| `forge_transport_transient` | Reaching the forge failed in a way a retry may clear |
| `fulcio_unavailable` | The certificate authority is unreachable or overloaded |
| `host_resolution_failed` | A registry host could not be resolved |
| `index_http_transient` | An index request failed in a way a retry may clear |
| `key_backend_unavailable` | The signing key backend is temporarily unavailable |
| `merge_request_unconfirmed` | The forge did not confirm the merge request in time |
| `observe_raced` | A tag appeared between the two reads of one announce |
| `oidc_token_unavailable` | The CI provider's OIDC token endpoint is unreachable or overloaded |
| `project_locked` | Another process holds the lock on ocx.toml |
| `prune_refused_pending` | The index still lists a tag prune would delete, pending an announce |
| `prune_tag_still_present` | A deleted tag is still present in the registry |
| `registry_transient` | A registry operation failed in a way a retry may clear |
| `resolve_timeout` | Resolving a binding timed out |
| `short_blob_read` | The registry ended a blob download early |
| `singleflight_capacity_exceeded` | Too many distinct operations were in flight at once |
| `singleflight_timeout` | Waiting for shared in-flight work timed out |
| `transparency_log_unavailable` | The transparency log is unavailable |

## `77` PermissionDenied {#exit-77}

The operation was refused for lack of permission.

- Error kind: `permission_denied`

| Slug | Meaning |
| --- | --- |
| `forge_publisher_not_allowlisted` | The publishing project is not on the index project's job-token allowlist |
| `forge_push_refused` | The forge refused the push |
| `identity_mismatch` | The certificate identity does not match the required signer |
| `identity_token_file_permissive` | The identity token file is readable by group or other |
| `issuer_mismatch` | The certificate OIDC issuer does not match the required issuer |
| `oidc_pre_check_failed` | The OIDC token failed a client-side check before reaching the certificate authority |
| `permission_denied` | The operating system refused access to a file or directory |
| `unsigned_rejected_by_policy` | An unsigned document was refused because a signature is required |

## `78` ConfigError {#exit-78}

Configuration is invalid or incomplete.

- Error kind: `config_error`

| Slug | Meaning |
| --- | --- |
| `ambiguous_extra_ca_certs` | Both extra_ca_certs and extra_ca_certs_pem are declared |
| `ambiguous_trust_root` | The payload declares both trusted_root and trusted_root_json |
| `auth_env_missing` | An environment variable the authentication type needs is not set |
| `binding_value_invalid` | A binding's value is not a valid identifier |
| `binding_value_missing_registry` | A binding's identifier names no registry |
| `ci_missing_env` | A CI environment variable the export needs is not set |
| `config_edit_too_large` | The config file being edited exceeds the allowed size |
| `config_file_too_large` | A config file exceeds the allowed size |
| `config_parse` | A config file does not parse |
| `credential_helper_not_on_path` | The configured credential helper is not on PATH |
| `credential_helper_unsafe_path` | The credential helper resolves to an unsafe path |
| `envrc_exists` | An .envrc already exists and --force was not given |
| `extra_ca_certs_pem_invalid` | The extra CA bundle inlined in the payload is not usable |
| `extra_ca_value_invalid` | The configured extra CA bundle value holds no usable certificate |
| `extra_ca_value_too_large` | The configured extra CA bundle value exceeds the allowed size |
| `forbidden_registry_target` | The rewritten registry resolves into a forbidden address range |
| `fulcio_bad_request` | The certificate authority rejected the signing request |
| `group_holds_direct_binding` | A group table holds a binding outside its tools table |
| `index_source_not_published` | The registry is not a published index source, so it has no catalog to regenerate |
| `invalid_auth_type` | The configured authentication type is not recognized |
| `invalid_env` | An OCX environment variable holds a value its declaration refuses |
| `invalid_index_url` | A configured index URL is invalid |
| `invalid_managed_config_source` | The managed-config source is not a valid OCI identifier |
| `invalid_toolchain_name_charset` | A group or binding name carries a character toolchain paths refuse |
| `invalid_trust_policy` | The payload declares an unusable trust-policy entry |
| `launch_identities_invalid` | The OCX_LAUNCH_IDENTITIES value a parent ocx forwarded is malformed |
| `lock_missing` | ocx.lock does not exist |
| `lock_repository_not_bare` | ocx.lock records a repository carrying a tag or digest |
| `managed_config_contains_managed_section` | The managed-config payload contains a [managed] section |
| `managed_config_invalid` | The managed-config settings are invalid, or a system lock refuses the change |
| `managed_config_key_by_path` | The payload declares a trust-policy key signer by path instead of inline |
| `managed_config_payload_invalid_toml` | The managed-config payload is not a valid config file |
| `managed_config_payload_too_large` | The managed-config payload exceeds the allowed size |
| `mirror_config_invalid` | A registry mirror setting is invalid |
| `no_credential_store` | No credential helper or store is available to save credentials in |
| `no_host_leaf` | ocx.lock pins no platform this host can run |
| `package_key_invalid` | A package key is not a valid identifier |
| `package_key_missing_registry` | A package key names no registry |
| `patch_config_invalid` | A patch-tier setting is invalid |
| `patch_project_config_unreadable` | The project config patches would freeze into is unreadable |
| `patch_snapshot_active` | Patch pins cannot advance while OCX_PATCH_SNAPSHOT is set |
| `plain_http_index_not_allowed` | An index URL uses plain HTTP for a host not allowed to |
| `project_env_invalid_key` | ocx.toml sets an invalid environment variable name |
| `project_env_invalid_separator` | An ocx.toml environment entry has an unusable list separator |
| `project_env_invalid_value` | An ocx.toml environment entry has a value of the wrong shape |
| `project_env_reserved_key` | ocx.toml sets a reserved OCX environment key |
| `project_env_separator_edged_value` | An ocx.toml list value starts or ends with its separator |
| `project_env_separator_on_non_list` | An ocx.toml environment entry sets a separator on a non-list value |
| `project_env_unknown_modifier` | An ocx.toml environment entry names an unknown modifier type |
| `project_env_unknown_value_field` | An ocx.toml environment entry table holds an unknown field |
| `project_file_too_large` | A project file exceeds the allowed size |
| `project_manifest_not_editable` | ocx.toml is not an editable TOML document |
| `project_toml_invalid` | ocx.toml or ocx.lock is not valid TOML of the expected shape |
| `project_toml_serialize` | A project file could not be serialized |
| `record_name_not_a_filename` | The record name is not a plain filename |
| `record_sink_symlink` | The execution-record sink is a symlink |
| `record_template_not_unique` | The record name template has no varying component |
| `record_template_unknown_placeholder` | The record name template names an unknown placeholder |
| `records_required_without_sink` | Execution records are required but no sink is configured |
| `rendered_config_too_large` | Persisting the extra CA bundle would exceed the config size limit |
| `reserved_group_name` | ocx.toml declares a reserved group name |
| `retired_env` | A retired OCX environment variable is set |
| `session_path_not_absolute` | A session PATH entry is not an absolute path |
| `session_path_not_utf8` | A session PATH entry is not valid UTF-8 |
| `session_path_unencodable` | A session PATH entry contains a character the host format cannot store |
| `shell_section_in_project` | ocx.toml declares a [shell] section, which only config.toml may hold |
| `ssrf_forbidden_target` | A registry host resolves to an address outside the trusted set |
| `system_config_invalid` | The system-wide config file is unusable |
| `toolchain_dir_group_or_world_writable` | The toolchain_dir directory is writable by other users |
| `toolchain_dir_inaccessible` | The toolchain_dir setting names a path that cannot be inspected |
| `toolchain_dir_inside_global_home` | The toolchain_dir setting resolves inside the global toolchain home |
| `toolchain_dir_is_containment_anchor` | The toolchain_dir setting names the home directory or OCX_HOME itself |
| `toolchain_dir_no_containment_anchor` | No home directory or OCX_HOME exists to contain toolchain_dir |
| `toolchain_dir_not_a_directory` | The toolchain_dir setting names something other than a directory |
| `toolchain_dir_not_owner_owned` | The toolchain_dir directory is not owned by the current user |
| `toolchain_dir_outside_home` | The toolchain_dir setting resolves outside the home directory and OCX_HOME |
| `toolchain_dir_parent_component` | The toolchain_dir setting contains a '..' component |
| `toolchain_dir_relative` | The toolchain_dir setting is a relative path |
| `toolchain_dir_system_prefix` | The toolchain_dir setting resolves to a system location |
| `toolchain_dir_unexpandable` | The toolchain_dir setting starts with a '~' that cannot be expanded |
| `toolchain_name_control_character` | A toolchain group or entry name contains a control character |
| `toolchain_name_empty` | A toolchain group or entry name is empty |
| `toolchain_name_path_prefix` | A toolchain group or entry name carries a path prefix |
| `toolchain_name_relative` | A toolchain group or entry name is a relative path component |
| `toolchain_name_separator` | A toolchain group or entry name contains a path separator |
| `toolchain_name_trailing_dot_or_space` | A toolchain group or entry name ends with a dot or a space |
| `trust_policy_invalid` | A trust policy entry is malformed |
| `trust_root_load` | Trust material failed to load |
| `trust_root_unavailable` | The trust root could not be loaded |
| `trusted_root_invalid` | The named file is not a usable Sigstore trusted root |
| `unknown_group_section` | A group table holds an unknown key |
| `unsupported_declaration_hash_version` | ocx.lock records a declaration hash version this build does not understand |
| `unsupported_lock_version` | ocx.lock has a version this build does not understand |
| `update_section_in_project` | ocx.toml declares an [update] section, which only config.toml may hold |

## `79` NotFound {#exit-79}

A named package, tag, file or resource does not exist.

- Error kind: `not_found`

| Slug | Meaning |
| --- | --- |
| `attestation_not_found` | No verified attestation matches the requested predicate type |
| `binding_not_declared` | The named binding is not declared in ocx.toml |
| `binding_not_found` | The named binding does not exist in ocx.toml |
| `blob_not_found` | The registry has no such blob |
| `build_receipt_not_found` | No build receipt exists beside the bundle |
| `config_file_not_found` | An explicitly named config or project file does not exist |
| `dependency_manifest_not_found` | A pinned dependency manifest is not in the registry |
| `dependency_not_found` | A dependency is not in the selected index |
| `index_manifest_not_found` | The registry has no manifest for a tag the index update walked |
| `managed_config_input_not_found` | A file the managed-config payload names does not exist |
| `managed_config_source_not_found` | The registry has no managed-config package at the configured source |
| `manifest_not_found` | The registry has no such manifest |
| `no_indexable_tag` | The repository carries no tag the index can record |
| `no_signatures_found` | The target has no signature referrers |
| `no_usable_bundle` | Referrers exist but none is a recognised Sigstore bundle |
| `not_in_index` | The package is not listed in the index |
| `owner_unknown` | The forge has no account with this login |
| `package_not_found` | The package does not exist |
| `patch_descriptor_vanished` | A pinned patch descriptor is no longer in the registry |
| `patch_snapshot_descriptor_missing` | The patch snapshot names a descriptor that is not stored locally |
| `repository_not_found` | The registry has no such repository |
| `required_path_missing` | A required path does not exist |
| `symlink_not_found` | The package has no candidate or current symlink to resolve |
| `tag_not_found` | The registry has no such tag |
| `target_not_an_index` | A platform was requested but the reference names a single manifest |
| `target_not_found` | The reference resolves to no manifest for the requested platform |
| `template_dependency_not_installed` | An interpolation token names a dependency that is not installed |
| `unclaimed_package` | The package has no index entry to announce into |
| `unresolved_tag` | A tag to announce does not exist in the registry |

## `80` AuthError {#exit-80}

Authentication failed or credentials are missing.

- Error kind: `auth_error`

| Slug | Meaning |
| --- | --- |
| `credential_helper_failed` | The credential helper failed to supply credentials |
| `docker_credential_retrieval` | Reading Docker credentials failed |
| `forge_auth_failed` | The forge rejected the request's credentials |
| `forge_credential_missing` | A forge write found no API credential to authenticate with |
| `forge_push_access_denied` | The credential may not push to the repository |
| `login_rejected` | The registry rejected the supplied credentials |
| `oidc_token_rejected` | The certificate authority rejected the OIDC token |
| `registry_auth_failed` | The registry rejected the request's authentication |

## `81` PolicyBlocked {#exit-81}

A local policy or safeguard refused the operation; loosen the flag or pass --force.

- Error kind: `permission_denied`

| Slug | Meaning |
| --- | --- |
| `credential_helper_not_installed` | The credential helper is not installed, and an ocx started by a credential helper never downloads |
| `frozen_refused` | The verb discovers new digests, which frozen mode forbids |
| `index_resolution_blocked` | A local policy refused resolving through the index |
| `offline_attest_refused` | Attaching was refused because offline mode is on |
| `offline_manifest_missing` | Offline mode and the manifest is not stored locally |
| `offline_mode` | A network operation was attempted in offline mode |
| `offline_sign_refused` | Signing was refused because offline mode is on |
| `patch_policy_blocked` | A local policy refused fetching a patch descriptor |
| `project_resolution_blocked` | A local policy refused resolving a binding |
| `prune_no_index` | No index is configured to check the tags against |
| `prune_refused_durable` | The index still lists a tag prune would delete, and the tag is durable |

## `82` Unsupported {#exit-82}

The operation as requested is not supported or not enabled by this registry, forge or build; retrying will not help.

- Error kind: `unsupported`

| Slug | Meaning |
| --- | --- |
| `forge_capability_unavailable` | The forge or project lacks a capability the transport needs |
| `referrers_unsupported` | The registry supports neither the OCI Referrers API nor a referrers fallback tag |
| `registry_delete_unsupported` | The registry does not delete tags |
| `unsupported_key_backend` | A key reference names a recognised key backend this build does not implement |
