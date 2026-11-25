#!/bin/sh
# Builds the static 64-bit atmos67 into target/packages-aarch64, to try on the
# Vero V in place of the packaged 32-bit /usr/bin/atmos67. Run it on an arm64
# Linux host, in the image built from packaging/Dockerfile.aarch64:
#
#   docker build -t atmos67-aarch64 -f packaging/Dockerfile.aarch64 packaging
#   docker run --rm -v "$PWD":/work atmos67-aarch64 packaging/build-aarch64.sh
set -eu
cd "$(dirname "$0")/.."

target=aarch64-unknown-linux-musl
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
out=target/packages-aarch64

cargo test --release --target "$target"
cargo build --release --target "$target"

mkdir -p "$out"
install -m 755 "target/$target/release/atmos67" "$out/atmos67-$version-aarch64"
strip "$out/atmos67-$version-aarch64"
file "$out/atmos67-$version-aarch64"
ls -l "$out"
