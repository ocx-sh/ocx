"""
Provides the module extension for loading OCX packages as Bazel repositories.
"""

load("@rules_oci//oci/private:authn.bzl", "authn")  # buildifier: disable=bzl-visibility
load("//ocx/private:platforms.bzl", "build_platform_name", "platform_from_oci")

_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
_ARTIFACT_MEDIA_TYPE = "application/vnd.sh.ocx.package.v1"
_CONFIG_MEDIA_TYPE = "application/vnd.sh.ocx.package.v1+json"
_LAYER_MEDIA_TYPE = "application/vnd.oci.image.layer.v1.tar+xz"
_DOWNLOAD_HEADERS = {
    "Accept": _INDEX_MEDIA_TYPE + "," + _MANIFEST_MEDIA_TYPE,
    "Docker-Distribution-API-Version": "registry/2.0",
}

def _sanitize(s):
    return s.replace("://", "_").replace("/", "_").replace(".", "_")

def _extract_sha256_from_digest(digest):
    if not digest.startswith("sha256:"):
        fail("unsupported digest format '{}'".format(digest))
    return digest.partition(":")[2]

def _build_image_repo_name(repository, platform_arch, platform_os, digest):
    sha256_short = _extract_sha256_from_digest(digest)[:12]
    return "{}_{}_{}_{}".format(repository, platform_arch, platform_os, sha256_short)

def _url_join(*parts):
    return "/".join([part.strip("/") for part in parts])

def _resolve_index(module_ctx, auth, registry, repository, tag, sha256 = ""):
    token = auth.get_token(registry, repository)
    registry_sanitized = _sanitize(registry)
    repository_sanitized = _sanitize(repository)
    index_file_name = "{}-{}.json".format(registry_sanitized, repository_sanitized)
    url = _url_join(registry, "v2", repository, "manifests", tag)

    module_ctx.download(url, index_file_name, auth = {url: token}, headers = _DOWNLOAD_HEADERS, sha256 = sha256)
    index = json.decode(module_ctx.read(index_file_name))
    if index["mediaType"] != _INDEX_MEDIA_TYPE:
        fail("invalid mediaType '{}'".format(index["mediaType"]))
    if index["artifactType"] != _ARTIFACT_MEDIA_TYPE:
        fail("invalid artifactType '{}'".format(index["artifactType"]))
    manifest_refs = []
    for manifest in index["manifests"]:
        if manifest["mediaType"] != _MANIFEST_MEDIA_TYPE:
            continue
        digest = manifest["digest"]
        platform = platform_from_oci(oci_arch = manifest["platform"]["architecture"], oci_os = manifest["platform"]["os"])
        if not platform:
            continue
        manifest_refs.append(
            struct(
                digest = digest,
                registry = registry,
                repository = repository,
                platform_arch = platform[0],
                platform_os = platform[1],
            ),
        )
    if not len(manifest_refs):
        fail("not usable manifest found")
    return manifest_refs

_IMAGE_REPO_MANIFEST_PATH = "manifest.json"
_IMAGE_REPO_LAYER_PATH = "layer"
_IMAGE_REPO_CONFIG_PATH = "config.json"

def _resolve_package(module_ctx, auth, registry, repository, digest):
    token = auth.get_token(registry, repository)

    # download manifest
    url = _url_join(registry, "v2", repository, "manifests", digest)
    module_ctx.download(
        url,
        _IMAGE_REPO_MANIFEST_PATH,
        auth = {url: token},
        headers = _DOWNLOAD_HEADERS,
        sha256 = _extract_sha256_from_digest(digest),
    )
    manifest = json.decode(module_ctx.read(_IMAGE_REPO_MANIFEST_PATH))
    if manifest["mediaType"] != _MANIFEST_MEDIA_TYPE:
        fail("invalid mediaType '{}'".format(manifest["mediaType"]))
    config = manifest["config"]
    if config["mediaType"] != _CONFIG_MEDIA_TYPE:
        fail("invalid config mediaType '{}'".format(config["mediaType"]))

    # load config
    config_digest = config["digest"]
    config_url = _url_join(registry, "v2", repository, "blobs", config_digest)
    module_ctx.download(
        config_url,
        _IMAGE_REPO_CONFIG_PATH,
        auth = {config_url: token},
        sha256 = _extract_sha256_from_digest(config_digest),
    )

    # download layer
    layer = manifest["layers"][0]
    if layer["mediaType"] != _LAYER_MEDIA_TYPE:
        fail("invalid layer mediaType '{}'".format(layer["mediaType"]))
    layer_digest = layer["digest"]
    layer_url = _url_join(registry, "v2", repository, "blobs", layer_digest)
    module_ctx.download_and_extract(
        layer_url,
        _IMAGE_REPO_LAYER_PATH,
        auth = {layer_url: token},
        sha256 = _extract_sha256_from_digest(layer_digest),
        type = "tar.xz",
    )

def _encode_manifests_as_facts(manifests):
    return json.encode([{
        "digest": manifest.digest,
        "registry": manifest.registry,
        "repository": manifest.repository,
        "platform_arch": manifest.platform_arch,
        "platform_os": manifest.platform_os,
    } for manifest in manifests])

def _decode_manifests_from_fact(facts):
    return [struct(
        digest = fact["digest"],
        registry = fact["registry"],
        repository = fact["repository"],
        platform_arch = fact["platform_arch"],
        platform_os = fact["platform_os"],
    ) for fact in json.decode(facts)]

_REPOSITORY_ATTR = struct(
    www_authenticate_challenges = {
        "ocx.sh": 'Bearer realm="https://ocx.sh/artifactory/api/docker/sh-ocx-oci-prod/v2/token",service="ocx.sh",scope="repository:shfmt:pull"',
    },
)

def build_extension(package_repo_impl, package_image_repo_impl, *, additional_attrs = None):
    """Build a new extension for using OCX-based packages.

    Args:
            package_repo_impl: Implementation function of the package repo rule
            package_image_repo_impl: Implementation function of the package image repo rule
            additional_attrs: Additional attributes that should be forwarded from the tag class to the repos
    Returns:
        tuple of the 1. extension, 2. the package image repo rule and 3. the package repo rule
    """

    def ocx_package_image_repo_impl(repository_ctx):
        """Repository rule implementation for the repository containing the image of a single OCX package."""

        # create a fake repo_context to inject constant attributes
        rctx = struct(
            attr = _REPOSITORY_ATTR,
            download = repository_ctx.download,
            file = repository_ctx.file,
            delete = repository_ctx.delete,
            read = repository_ctx.read,
            execute = repository_ctx.execute,
            os = repository_ctx.os,
            which = repository_ctx.which,
        )
        auth = authn.new(rctx)
        _resolve_package(repository_ctx, auth, repository_ctx.attr.registry, repository_ctx.attr.repository, repository_ctx.attr.digest)
        package_image_repo_impl(repository_ctx, _IMAGE_REPO_LAYER_PATH, _IMAGE_REPO_CONFIG_PATH)

    attrs = {
        "registry": attr.string(mandatory = True),
        "repository": attr.string(mandatory = True),
        "digest": attr.string(mandatory = True),
        "arch": attr.string(mandatory = True),
        "os": attr.string(mandatory = True),
    }
    if additional_attrs:
        attrs.update(additional_attrs)
    ocx_package_image_repo = repository_rule(
        implementation = ocx_package_image_repo_impl,
        attrs = attrs,
    )

    def ocx_package_repo_impl(repository_ctx):
        """Repository rule implementation for the 'hub' of a single OCX package, redirecting to the correct image repository based on the platform."""
        package_repo_impl(repository_ctx, repository_ctx.attr.images_by_platform)

    attrs = {
        "images_by_platform": attr.string_dict(mandatory = True),
    }
    if additional_attrs:
        attrs.update(additional_attrs)
    ocx_package_repo = repository_rule(
        implementation = ocx_package_repo_impl,
        attrs = attrs,
    )

    def ocx_impl(module_ctx):
        # create a fake repository_context to use the authn library outside of repository rules
        rctx = struct(
            attr = _REPOSITORY_ATTR,
            download = module_ctx.download,
            file = module_ctx.file,
            delete = lambda path: module_ctx.execute(["rm", path]),
            read = module_ctx.read,
            execute = module_ctx.execute,
            os = module_ctx.os,
            which = module_ctx.which,
        )
        auth = authn.new(rctx)
        all_manifests = []
        facts = {}

        def handle_package(manifests, additional_attrs_values):
            image_repos_by_platform = {}
            for manifest in manifests:
                image_repo_name = _build_image_repo_name(manifest.repository, manifest.platform_arch, manifest.platform_os, manifest.digest)
                image_repos_by_platform[build_platform_name(manifest.platform_arch, manifest.platform_os)] = image_repo_name

                if manifest not in all_manifests:
                    all_manifests.append(manifest)

                    # create repository for the image of this manifest, if not already created
                    ocx_package_image_repo(
                        name = image_repo_name,
                        digest = manifest.digest,
                        registry = manifest.registry,
                        repository = manifest.repository,
                        arch = manifest.platform_arch,
                        os = manifest.platform_os,
                        **additional_attrs_values
                    )

            ocx_package_repo(
                name = package.name or package.repository,
                images_by_platform = image_repos_by_platform,
                **additional_attrs_values
            )

        for mod in module_ctx.modules:
            for package in mod.tags.package:
                tag_is_digest = package.tag.startswith("sha256:")
                if tag_is_digest:
                    index_sha256 = _extract_sha256_from_digest(package.tag)
                    manifests = _resolve_index(module_ctx, auth, package.registry, package.repository, package.tag, sha256 = index_sha256)
                elif package.local_index_path:
                    expected_index_suffix = "/index/" + package.registry.removeprefix("https://") + "/tags/" + package.repository + ".json"
                    if not package.local_index_path.name.endswith(expected_index_suffix):
                        fail("Invalid index location. Expecting '{}' to end with '{}'".format(package.local_index_path.name, expected_index_suffix))
                    index_of_indexes = json.decode(module_ctx.read(package.local_index_path))
                    index_digest = index_of_indexes.get(package.tag)
                    if not index_digest:
                        fail("tag '{}' not found in index '{}'".format(package.tag, package.local_index_path))
                    index_sha256 = _extract_sha256_from_digest(index_digest)
                    manifests = _resolve_index(module_ctx, auth, package.registry, package.repository, package.tag, sha256 = index_sha256)
                else:
                    canonical_package_name = package.registry + "/" + package.repository + ":" + package.tag
                    existing_package_facts = module_ctx.facts.get(canonical_package_name)
                    if existing_package_facts:
                        manifests = _decode_manifests_from_fact(existing_package_facts)
                        facts[canonical_package_name] = existing_package_facts
                    else:
                        manifests = _resolve_index(module_ctx, auth, package.registry, package.repository, package.tag)
                        facts[canonical_package_name] = _encode_manifests_as_facts(manifests)
                additional_attrs_values = {}
                if additional_attrs:
                    for additional_attr_key in additional_attrs.keys():
                        additional_attrs_values[additional_attr_key] = getattr(package, additional_attr_key)
                handle_package(manifests, additional_attrs_values)
        return module_ctx.extension_metadata(
            facts = facts,
        )

    attrs = {
        "name": attr.string(doc = "Name of the Bazel repository to create for this package. If not set, the repository will be named after the OCI repository."),
        "repository": attr.string(mandatory = True, doc = "OCI repository name, e.g. 'uv'"),
        "registry": attr.string(default = "https://ocx.sh", doc = "OCI registry URL, e.g. 'https://ocx.sh'"),
        "tag": attr.string(default = "latest", doc = "OCI tag, e.g. 'latest'"),
        "local_index_path": attr.label(doc = "Path to a local file containing the index for this package.", allow_single_file = [".json"]),
    }
    if additional_attrs:
        attrs.update(additional_attrs)
    package = tag_class(attrs = attrs)

    extension = module_extension(
        implementation = ocx_impl,
        tag_classes = {"package": package},
        doc = "Module extension for loading a OCX package as Bazel repo.",
    )

    return extension, ocx_package_repo, ocx_package_image_repo
