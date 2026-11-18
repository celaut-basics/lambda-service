# linux/amd64 variant of the lambda parent. nodo builds it with --opt platform=linux/amd64 (from
# service.json); the rust:alpine pin (the digest of its multi-arch index) resolves to the
# linux/amd64 image, so cargo emits a native static (musl) binary for it.
#
# The image: the static binary, the directory that it exports to its children (/lambda),
# and the packed child that the packer adds (service/__services__, __metadata__,
# __block__, .dependencies) under /app. No shell, no libc.
FROM rust:1.99.0-alpine3.22@sha256:c3a5ad77ff2e5ec99fffaf62cb518e61a9dcbd9e1d7526d281a380db0be3bb3b AS build
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY service/Cargo.toml service/Cargo.lock ./
COPY service/src ./src
RUN cargo build --release --locked \
 && mkdir -p /out/lambda /out/app \
 && install -m 0755 target/release/lambda-parent /out/lambda-parent \
 && sha256sum /out/lambda-parent
COPY service/__services__ /out/app/__services__
COPY service/__metadata__ /out/app/__metadata__
COPY service/__block__ /out/app/__block__
COPY service/.dependencies /out/app/.dependencies
# Every mtime to 0: the packer records them in the service, so without this each pack of
# the same tree gives another id.
RUN find /out -exec touch -d @0 {} +

FROM scratch
COPY --from=build /out/ /
