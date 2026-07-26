# Vendored specification artifacts

These files are **not ours**. They are pinned copies of upstream specifications
that OCX borrows vocabulary from, vendored so
`borrowed_vocabulary_matches_spec.rs` can assert our published schemas against
the real thing instead of against a hand-typed allowlist that drifts.

Nothing here is edited by hand. Refresh with `task website:schema:specs:refresh`
(see `website/schema.taskfile.yml`); that task refuses to overwrite a file whose
checksum has moved unless invoked as `FORCE=1 task website:schema:specs:refresh`,
because a changed checksum at a pinned tag means upstream moved under us and a
human has to decide what that implies.

## Provenance

| File | Upstream | Version | Retrieved |
|---|---|---|---|
| `oci/image-index-schema.json` | [`opencontainers/image-spec` `schema/image-index-schema.json`](https://raw.githubusercontent.com/opencontainers/image-spec/v1.1.1/schema/image-index-schema.json) | image-spec `v1.1.1` | 2026-09-04 |
| `in-toto/resource_descriptor.proto` | [`in-toto/attestation` `protos/in_toto_attestation/v1/resource_descriptor.proto`](https://raw.githubusercontent.com/in-toto/attestation/v1.1.0/protos/in_toto_attestation/v1/resource_descriptor.proto) | attestation `v1.1.0` | 2026-09-04 |
| `in-toto/resource_descriptor.fields.json` | generated from the `in-toto-attestation` Python package's protobuf descriptor | package `0.9.3` | 2026-09-04 |

## Notes on the pins

**OCI.** Only `properties.manifests.items.properties.platform` is consumed. That
object is self-contained: unlike its siblings in the same file it carries no
`$ref`, so no further image-spec files are vendored. Its property set is
`architecture`, `os`, `os.version`, `os.features`, `variant`, with
`architecture` and `os` required. There is no `features` property — image-spec
removed it, which is the drift this vendored copy exists to catch.

**in-toto.** The reference implementation is the pin, not the prose spec: the
Python package `in-toto-attestation` is already a test dependency of `test/`
(locked in `test/uv.lock`), so the field list is generated from its protobuf
descriptor rather than parsed out of a document. The proto text is vendored
alongside it for humans only — no test reads it.

The proto is taken at tag `v1.1.0`. The tag `v1.1` named in the original task
does not exist in that repository; the tags are `v1.0`, `v1.0.1`, `v1.0.2`,
`v1.1.0`, `v1.1.1`, `v1.1.2`, `v1.2.0`. `v1.1.0` is the closest existing tag to
that name, and the file is byte-identical at `v1.0.1` (the tag nearest the
Python package's 0.9.3 release date), so the choice does not change the
vocabulary — verified by comparing both downloads.

`type` in the fields file is the protobuf field-type number:
`9` = `TYPE_STRING`, `11` = `TYPE_MESSAGE`, `12` = `TYPE_BYTES`.

## Checksums

`sha256sum -c` format, paths relative to this directory. Rewritten only by the
refresh task.

<!-- sha256:begin -->
```
1a4641a610933fac77db9af7a83664d54883efac180f8867efd0497048e1a82e  oci/image-index-schema.json
246a5cf0ac8cc1223443aeb969e337ac12fd5ac1f062b90ba9170a6cdb83855f  in-toto/resource_descriptor.proto
22564ab95f0ca11434fa68beb77751b41a13e798d2cdf2d9a247e3537778e038  in-toto/resource_descriptor.fields.json
```
<!-- sha256:end -->
