#!/bin/sh
# Compiles pgdog inside a builder image (see packaging/builder).
#   compile.sh <out-dir>
# Run from the repository root.
set -eu

out="$1"
# RUSTFLAGS replaces the mold linker flags .cargo/config.toml sets for
# development; release packages use the default linker.
export RUSTFLAGS="${RUSTFLAGS:-} -C strip=symbols"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc
cargo build --release --locked -p pgdog --bin pgdog
target="${CARGO_TARGET_DIR:-target}"
install -D -m 0755 "$target/release/pgdog" "$out/pgdog"
"$out/pgdog" --version
