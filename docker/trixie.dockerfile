# Official ocx image — Debian trixie (glibc) variant, the default.
# The binary is extracted from the cargo-dist release archive by
# .github/workflows/docker-publish.yml — never compiled here.
FROM debian:trixie-slim@sha256:28de0877c2189802884ccd20f15ee41c203573bd87bb6b883f5f46362d24c5c2
ARG TARGETARCH
COPY --chmod=755 binaries/${TARGETARCH}/ocx /usr/local/bin/ocx
# The image pins its ocx version — the self-update notice is pure noise here.
ENV OCX_NO_UPDATE_CHECK=1
CMD ["ocx"]
