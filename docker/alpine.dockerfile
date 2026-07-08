# Official ocx image — Alpine variant (static musl binary).
# The binary is extracted from the cargo-dist release archive by
# .github/workflows/docker-publish.yml — never compiled here.
FROM alpine:3@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
ARG TARGETARCH
COPY --chmod=755 binaries/${TARGETARCH}/ocx /usr/local/bin/ocx
# The image pins its ocx version — the self-update notice is pure noise here.
ENV OCX_NO_UPDATE_CHECK=1
CMD ["ocx"]
