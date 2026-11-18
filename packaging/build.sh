#!/bin/sh
# Builds the OSMC (Vero V) package and the Kodi add-on into target/packages.
# Run it in the image built from packaging/Dockerfile, from the repository:
#
#   docker build -t atmos67-armhf packaging
#   docker run --rm -v "$PWD":/work atmos67-armhf packaging/build.sh
#
# The tests run the armhf binary, so the kernel needs qemu registered for
# 32-bit ARM (Docker Desktop has it; elsewhere
# `docker run --privileged --rm tonistiigi/binfmt --install arm`).
set -eu
cd "$(dirname "$0")/.."

target=armv7-unknown-linux-gnueabihf
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
out=target/packages

cargo test --release --target "$target"
cargo build --release --target "$target"

stage=$(mktemp -d)
cp -R packaging/deb "$stage/deb"
cp -R packaging/kodi/service.atmos67 "$stage/service.atmos67"
find "$stage" -name .DS_Store -delete
find "$stage" -name __pycache__ -prune -exec rm -rf {} +
sed -i "s/@VERSION@/$version/" "$stage/deb/DEBIAN/control" "$stage/service.atmos67/addon.xml"

install -D -m 755 "target/$target/release/atmos67" "$stage/deb/usr/bin/atmos67"
arm-linux-gnueabihf-strip "$stage/deb/usr/bin/atmos67"
rm -rf "$out"
mkdir -p "$out"
dpkg-deb --root-owner-group --build "$stage/deb" "$out/atmos67_${version}_armhf.deb"
(cd "$stage" && zip -qr service.zip service.atmos67)
cp "$stage/service.zip" "$out/service.atmos67-$version.zip"
rm -rf "$stage"
ls -l "$out"
