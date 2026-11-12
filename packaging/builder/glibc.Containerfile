# Builds the binary for the .deb and .rpm packages.
#
# Rocky Linux 9 is the oldest distribution we support: glibc 2.34 and
# OpenSSL 3. A binary linked here runs on everything newer (Debian 12+,
# Ubuntu 22.04+, RHEL/Rocky/Alma 9+).
FROM docker.io/library/rust:1.96-slim-bookworm AS rust

FROM docker.io/rockylinux/rockylinux:9

RUN dnf -y install --setopt=install_weak_deps=False \
        gcc gcc-c++ make cmake clang clang-libs perl git pkgconf-pkg-config \
        openssl-devel zlib-devel \
        bison flex libicu-devel readline-devel \
    && dnf clean all

# The official toolchain: targets glibc 2.17, runs here.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN cargo --version
