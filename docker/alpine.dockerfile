# Official ocx image — Alpine variant (static musl binary).
# The binary is extracted from the cargo-dist release archive by
# .github/workflows/docker-publish.yml — never compiled here.
FROM alpine:3@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
ARG TARGETARCH
COPY --chmod=755 binaries/${TARGETARCH}/ocx /usr/local/bin/ocx
# The image pins its ocx version — the self-update notice is pure noise here.
ENV OCX_NO_UPDATE_CHECK=1
CMD ["ocx"]
