# Official ocx image — Debian trixie (glibc) variant, the default.
# The binary is extracted from the cargo-dist release archive by
# .github/workflows/docker-publish.yml — never compiled here.
FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
ARG TARGETARCH
COPY --chmod=755 binaries/${TARGETARCH}/ocx /usr/local/bin/ocx
# The image pins its ocx version — the self-update notice is pure noise here.
ENV OCX_NO_UPDATE_CHECK=1
CMD ["ocx"]
