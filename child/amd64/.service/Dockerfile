# linux/amd64 variant of the lambda child. nodo builds it with --opt platform=linux/amd64 (from
# service.json), so the alpine pin (the digest of its multi-arch index) resolves to the
# linux/amd64 image and gcc emits a native binary for it.
#
# The image is two files and one directory: a static MicroPython (unix port, musl,
# stripped) and the entry script. No shell, no libc, no busybox (the node's initramfs has
# its own and leaves it behind at switch_root).
#
# Pinned: the MicroPython source (tag v1.29.0, GitHub archive, sha256) and the alpine
# image (index digest). The apk packages (gcc, musl-dev, make, python3) come from
# alpine 3.22 at build time: the main repository keeps one version per package, so a
# version pin would stop resolving at the next security update.
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS build
RUN apk add --no-cache build-base python3
ARG MP_SHA256=8072f6ad42c0e3bbfca88ed09bf8f1106ce207a39129af920f70081b8f2fe286
RUN wget -q -O /mp.tar.gz https://github.com/micropython/micropython/archive/refs/tags/v1.29.0.tar.gz \
 && echo "$MP_SHA256  /mp.tar.gz" | sha256sum -c -
# The heap of the unix port is 2 MiB on 64-bit, and -X heapsize cannot be given (the node
# runs the entry point with no arguments, and a shebang passes only one). 8 MiB here.
# Unused heap pages are never touched, so the RSS grows only with what a function uses.
RUN tar -xzf /mp.tar.gz -C / \
 && cd /micropython-1.29.0 \
 && sed -i 's|^long heap_size = 1024 \* 1024 \* (sizeof(mp_uint_t) / 4);|long heap_size = 8 * 1024 * 1024;|' ports/unix/main.c \
 && grep -q '^long heap_size = 8 \* 1024 \* 1024;' ports/unix/main.c \
 && make -C ports/unix -j"$(nproc)" VARIANT=standard FROZEN_MANIFEST= \
      MICROPY_USE_READLINE=0 MICROPY_PY_BTREE=0 MICROPY_PY_THREAD=0 MICROPY_PY_TERMIOS=0 \
      MICROPY_PY_SOCKET=0 MICROPY_PY_FFI=0 MICROPY_PY_SSL=0 MICROPY_VFS_FAT=0 \
      MICROPY_VFS_LFS1=0 MICROPY_VFS_LFS2=0 LDFLAGS_EXTRA=-static \
 && mkdir -p /out/lambda \
 && install -m 0755 -s ports/unix/build-standard/micropython /out/micropython \
 && sha256sum /out/micropython
COPY service/lambda.py /out/lambda.py
# Every mtime to 0: the packer records them in the service, so without this each pack of
# the same tree gives another id.
RUN chmod 0755 /out/lambda.py /out/lambda && find /out -exec touch -d @0 {} +

FROM scratch
COPY --from=build /out/ /
