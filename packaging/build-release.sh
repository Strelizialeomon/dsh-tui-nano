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
need rsync
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
manifest_arch="$(node -e 'process.stdout.write(require(process.argv[1]).arch)' "$manifest")"
if [ "$arch" != "$manifest_arch" ]; then
  echo "build-release: target arch $arch != manifest arch $manifest_arch" >&2
  exit 2
fi
name="dsh-tui-nano-$version-$arch"
stage="$here/dist/$name"

target_dir="${CARGO_TARGET_DIR:-$repo/target}"
case "$target_dir" in
  /*) ;;
  *) target_dir="$repo/$target_dir" ;;
esac

echo "build-release: cargo build --release -p nano-app"
cargo build --release -p nano-app --manifest-path "$repo/Cargo.toml" --target-dir "$target_dir"

binary="$target_dir/release/nano"
if [ ! -x "$binary" ]; then
  echo "build-release: binary not found: $binary" >&2
  exit 2
fi
if ! file "$binary" | grep -q "$arch"; then
  echo "build-release: binary architecture does not match $arch: $(file "$binary")" >&2
  exit 2
fi

rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/lib"

cp "$binary" "$stage/bin/nano"
chmod 0755 "$stage/bin/nano"

# Payload whitelist: source, manifests, and the bundle patch. Dependencies are
# declared in package.json and installed by the official profile installer.
# Dotfiles, tests, lockfiles, and any unrelated working-tree file stay out.
rsync -a --prune-empty-dirs \
  --exclude='test/' \
  --exclude='test/**' \
  --include='*/' \
  --include='*.mjs' \
  --include='package.json' \
  --include='cordis.patch.yml' \
  --include='README.md' \
  --exclude='*' \
  "$repo/bridge/" "$stage/lib/bridge/"
rsync -a --prune-empty-dirs \
  --include='*/' \
  --include='*.mjs' \
  --include='*.json' \
  --include='README.md' \
  --exclude='*' \
  "$repo/protocol/" "$stage/lib/protocol/"

cp "$here/install.sh" "$stage/install.sh"
cp "$here/uninstall.sh" "$stage/uninstall.sh"
cp "$here/manifest.json" "$stage/manifest.json"
cp "$here/README.md" "$stage/README.md"
chmod 0755 "$stage/install.sh" "$stage/uninstall.sh"

# Refuse to ship a payload that contains secret-like files.
if find "$stage" -type f \( -name '.env*' -o -name '*.pem' -o -name '*.key' -o -name 'id_rsa*' \) | grep -q .; then
  echo "build-release: payload contains a secret-like file; refusing to build" >&2
  find "$stage" -type f \( -name '.env*' -o -name '*.pem' -o -name '*.key' -o -name 'id_rsa*' \) >&2
  exit 2
fi

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
