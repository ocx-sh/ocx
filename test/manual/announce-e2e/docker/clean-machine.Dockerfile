# Clean-machine proof for the Track D announce E2E gate: a host that has never
# seen ocx resolves `<ns>/<pkg>` from the rendered index.ocx.sh.
#
# Deliberately NOT `test/docker/*.Dockerfile`'s `__testing` binary — that build
# unlocks internal seams for this repo's acceptance suite, so using it here
# would stop proving what a real user's machine does (Key Decision D-7).
#
# The binary is staged into the build context by `clean_install_check.sh`
# rather than downloaded from a GitHub release, because index-kind resolution
# is not in a release yet: ocx 0.4.3 rejects the config this image writes with
# `unknown field 'index', expected 'url'`, and `ocx self setup` would pull that
# same released copy over the staged one. A plain `--release` build is still a
# real user's artifact — the `__testing` prohibition is what D-7 is about.
# trixie, not bookworm: a locally- or CI-built ocx links against the builder's
# glibc (2.39 on a current runner), and bookworm's 2.36 cannot load it.
# Released artifacts are built for older glibc and would run on either.
FROM debian:trixie-slim

ARG E2E_NAMESPACE
ARG E2E_PACKAGE
ARG INDEX_SITE=https://index.ocx.sh
# The identifier prefix `<ns>/<pkg>` resolves under, which is what a
# `[registries]` key matches — see the config note below.
ARG E2E_INDEX_PREFIX=ocx.sh

# TLS roots for index.ocx.sh and the registry it points at. Nothing else: a
# clean machine is the point.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY ocx /usr/local/bin/ocx

# Resolve through the ocx-index protocol. `ocx.sh` is not index-kind by
# default yet (register §6), so this says so explicitly.
#
# The key is the IDENTIFIER PREFIX (`ocx.sh`), not the namespace: a
# `[registries]` key is matched against the registry component of the resolved
# identifier, and `<ns>/<pkg>` resolves to `ocx.sh/<ns>/<pkg>`. Keying it on
# `<ns>` parses fine and then never matches — zero index sources get built and
# ocx falls back to plain-OCI against `ocx.sh/v2/` with no error, which reads
# as "the index did not serve". Field name/shape per
# website/src/docs/reference/configuration.md § [registries.<name>] / index.
RUN mkdir -p /root/.ocx \
    && printf '[registries."%s"]\nindex = "%s"\n' "$E2E_INDEX_PREFIX" "$INDEX_SITE" \
    > /root/.ocx/config.toml

ENV E2E_NAMESPACE=${E2E_NAMESPACE} E2E_PACKAGE=${E2E_PACKAGE}

# `--format` is a root flag; `install` lives under the `package` group.
ENTRYPOINT ["sh", "-c", "ocx --format json package install \"$E2E_NAMESPACE/$E2E_PACKAGE\""]
