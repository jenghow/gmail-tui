#!/usr/bin/env bash
# Build the release binary and package it (binary + README + LICENSE) as a zip
# in dist/, with a matching SHA-256 checksum. Packages the current host only.
set -euo pipefail

# Repo root is the parent of this script's directory.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

pkg="$(sed -n 's/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
if [ -z "$pkg" ]; then
    echo "error: could not read the package name from Cargo.toml" >&2
    exit 1
fi
if [ -z "$version" ]; then
    echo "error: could not read the version from Cargo.toml" >&2
    exit 1
fi

if ! command -v zip >/dev/null 2>&1; then
    echo "error: 'zip' not found; install it (e.g. 'apt install zip' or 'brew install zip')" >&2
    exit 1
fi

triple="$(rustc -vV | sed -n 's/^host: //p')"

echo "Building $pkg $version for $triple..."
cargo build --release --locked

# Git-Bash and Windows targets carry a .exe suffix; unix targets do not.
exe="$pkg"
if [ -f "target/release/$pkg.exe" ]; then
    exe="$pkg.exe"
fi
binary="target/release/$exe"
if [ ! -f "$binary" ]; then
    echo "error: missing build output: $binary" >&2
    exit 1
fi

stem="$pkg-$version-$triple"
dist="dist"
stage="$dist/$stem"
rm -rf "$stage"
mkdir -p "$stage"

cp "$binary" "$stage/"
for doc in README.md LICENSE; do
    if [ -f "$doc" ]; then
        cp "$doc" "$stage/"
    fi
done

rm -f "$dist/$stem.zip"
# Zip from inside dist/ so the staging folder is the archive's top-level entry.
(cd "$dist" && zip -r -q "$stem.zip" "$stem")

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$dist" && sha256sum "$stem.zip" > "$stem.zip.sha256")
elif command -v shasum >/dev/null 2>&1; then
    (cd "$dist" && shasum -a 256 "$stem.zip" > "$stem.zip.sha256")
else
    echo "warning: neither sha256sum nor shasum found; skipping checksum" >&2
fi

rm -rf "$stage"
echo "Wrote $dist/$stem.zip"
