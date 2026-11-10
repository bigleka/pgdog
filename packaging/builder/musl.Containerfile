# Builds the binary for the .apk package, against Alpine's musl.
#
# Linked dynamically (-crt-static): a static musl binary can't dlopen()
# plugins.
FROM docker.io/library/rust:1.91-alpine3.22

RUN apk add --no-cache build-base cmake clang-dev perl git pkgconf \
        openssl-dev linux-headers

ENV RUSTFLAGS="-C target-feature=-crt-static"
