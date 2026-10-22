#!/bin/bash
# Build and stage the macOS release payload.
#
# The payload contains the Rust binary, the thin bridge package, the protocol
# package, the installer/uninstaller, and the versioned manifest. It never
# contains credentials, sessions, or user configuration.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
manifest="$here/manifest.json"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "build-release: missing required tool: $1" >&2
    exit 2
  fi
}
need cargo
need node
need shasum
need tar

version="$(node -e 'process.stdout.write(require(process.argv[1]).version)' "$manifest")"
bridge_version="$(node -e 'process.stdout.write(require(process.argv[1]).bridgeVersion)' "$manifest")"
package_bridge_version="$(node -e 'process.stdout.write(require(process.argv[1]).version)' "$repo/bridge/package.json")"
if [ "$bridge_version" != "$package_bridge_version" ]; then
  echo "build-release: manifest bridgeVersion $bridge_version != bridge/package.json $package_bridge_version" >&2
  exit 2
fi

arch="${NANO_RELEASE_ARCH:-$(uname -m)}"
name="dsh-tui-nano-$version-$arch"
stage="$here/dist/$name"

echo "build-release: cargo build --release -p nano-app"
cargo build --release -p nano-app --manifest-path "$repo/Cargo.toml"

binary="$repo/target/release/nano"
if [ ! -x "$binary" ]; then
  echo "build-release: binary not found: $binary" >&2
  exit 2
fi

rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/lib"

cp "$binary" "$stage/bin/nano"
chmod 0755 "$stage/bin/nano"

# Bridge package: source, manifest, and the bundle patch. Dependencies are
# declared in package.json and installed by the official profile installer.
rsync -a \
  --exclude node_modules \
  --exclude test \
  --exclude 'package-lock.json' \
  "$repo/bridge/" "$stage/lib/bridge/"
cp -R "$repo/protocol" "$stage/lib/protocol"
rm -rf "$stage/lib/protocol/node_modules"

cp "$here/install.sh" "$stage/install.sh"
cp "$here/uninstall.sh" "$stage/uninstall.sh"
cp "$here/manifest.json" "$stage/manifest.json"
cp "$here/README.md" "$stage/README.md"
chmod 0755 "$stage/install.sh" "$stage/uninstall.sh"

# Record every payload file so install/reinstall can verify the artifact.
(
  cd "$stage"
  find . -type f ! -name SHA256SUMS -print | LC_ALL=C sort | while IFS= read -r file; do
    shasum -a 256 "$file"
  done > SHA256SUMS
)

archive="$here/dist/$name.tar.gz"
rm -f "$archive"
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$here/dist" "$name"

echo "payload: $stage"
echo "archive: $archive"
echo "archive-sha256: $(shasum -a 256 "$archive" | awk '{print $1}')"
