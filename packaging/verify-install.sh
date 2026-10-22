#!/bin/bash
# Exercise install, reinstall, version refusal, startup refusal, and uninstall
# against a synthetic HOME. Never touches the caller's real home or credentials.
#
# Usage: packaging/verify-install.sh [--payload <dir>]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
payload="$here/dist/dsh-tui-nano-0.1.0-arm64"
if [ "${1:-}" = "--payload" ]; then
  payload="$2"
fi
[ -f "$payload/install.sh" ] || { echo "missing payload: $payload" >&2; exit 2; }

root="$(mktemp -d /tmp/nano-s13-install-XXXXXX)"
cleanup() { rm -rf "$root"; }
trap cleanup EXIT

export HOME="$root/home"
export DSH_HOME="$root/dsh-home"
export NANO_DATA_ROOT="$root/data"
export NANO_INSTALL_ROOT="$root/install"
export NANO_BIN_DIR="$root/bin"
mkdir -p "$HOME" "$DSH_HOME/profiles/other-product" "$NANO_BIN_DIR"

fail=0
check() {
  local label="$1"; shift
  if "$@"; then
    echo "CHECK ok   $label"
  else
    echo "CHECK fail $label"
    fail=1
  fi
}

snapshot() {
  # All files under DSH_HOME except the Nano profile, as path+hash pairs.
  ( cd "$DSH_HOME" && find . -type f ! -path './profiles/dsh-tui-nano/*' -print | LC_ALL=C sort | while IFS= read -r file; do
      shasum -a 256 "$file"
    done )
}

# Pre-existing user configuration, credentials, and another product's profile.
printf '{"sentinel":"keep"}\n' > "$DSH_HOME/config.json"
printf '{"credential":"keep"}\n' > "$DSH_HOME/credentials.json"
printf '{"name":"other-product"}\n' > "$DSH_HOME/profiles/other-product/package.json"
printf 'other: true\n' > "$DSH_HOME/profiles/other-product/cordis.yml"
before="$(snapshot)"

echo "=== install (first) ==="
"$payload/install.sh" --json > "$root/install-1.json"
cat "$root/install-1.json"

check "entrypoint exists" test -x "$NANO_BIN_DIR/nano"
check "receipt exists" test -f "$NANO_INSTALL_ROOT/install-receipt.json"
check "profile composed" grep -q '@dsh-tui-nano/bridge' "$DSH_HOME/profiles/dsh-tui-nano/package.json"
check "first install action" test "$(node -e 'process.stdout.write(require(process.argv[1]).payloadAction)' "$root/install-1.json")" = "first-install"
check "first install did not replace" test "$(node -e 'process.stdout.write(String(require(process.argv[1]).replacedExistingFiles))' "$root/install-1.json")" = "false"
check "data root is 0700" test "$(stat -f '%Lp' "$NANO_DATA_ROOT")" = "700"
check "config and credentials untouched" test "$(snapshot)" = "$before"
check "other profile untouched" grep -q '"name":"other-product"' "$DSH_HOME/profiles/other-product/package.json"

echo "=== install (reinstall) ==="
"$payload/install.sh" --json > "$root/install-2.json"
cat "$root/install-2.json"
check "reinstall leaves config and credentials untouched" test "$(snapshot)" = "$before"
check "reinstall keeps entrypoint" test -x "$NANO_BIN_DIR/nano"
check "reinstall action" test "$(node -e 'process.stdout.write(require(process.argv[1]).payloadAction)' "$root/install-2.json")" = "reused"

echo "=== explicit path flags are frozen into the wrapper ==="
alt_dsh_home="$root/alt-dsh-home"
alt_data="$root/alt-data"
alt_install="$root/alt-install"
alt_bin="$root/alt-bin"
"$payload/install.sh" --dsh-home "$alt_dsh_home" --data-root "$alt_data" --install-root "$alt_install" --bin-dir "$alt_bin" --no-profile-install --json > "$root/install-alt.json"
check "alt entrypoint exists" test -x "$alt_bin/nano"
check "wrapper exports DSH_HOME" grep -q "export DSH_HOME='$alt_dsh_home'" "$alt_bin/nano"
check "wrapper exports NANO_DATA_ROOT" grep -q "export NANO_DATA_ROOT='$alt_data'" "$alt_bin/nano"
check "wrapper exports NANO_DSH" grep -q "export NANO_DSH='" "$alt_bin/nano"
"$alt_bin/nano" --help > "$root/alt-help.txt"
check "alt wrapper runs" grep -q '用法' "$root/alt-help.txt"
"$payload/uninstall.sh" --install-root "$alt_install" --bin-dir "$alt_bin" --json >/dev/null
check "alt entrypoint removed" test ! -e "$alt_bin/nano"

echo "=== version refusal ==="
fake_dsh="$root/fake-dsh"
cat > "$fake_dsh" <<'FAKE'
#!/bin/sh
echo "0.2.1-rc.1"
FAKE
chmod 755 "$fake_dsh"
set +e
NANO_INSTALL_ROOT="$root/refused-install" "$payload/install.sh" --dsh-bin "$fake_dsh" >"$root/refused.txt" 2>&1
refused_status=$?
set -e
cat "$root/refused.txt"
check "version mismatch exits non-zero" test "$refused_status" -ne 0
check "version mismatch names the expected version" grep -q '0.2.1-rc.2' "$root/refused.txt"
check "version mismatch did not create the refused root" test ! -e "$root/refused-install"

echo "=== startup refusal (Rust preflight) ==="
set +e
NANO_DSH="$fake_dsh" "$NANO_BIN_DIR/nano" --no-install --dsh-bin "$fake_dsh" --data-root "$NANO_DATA_ROOT" >"$root/startup.txt" 2>&1
startup_status=$?
set -e
cat "$root/startup.txt"
check "startup refuses an unknown official version" grep -q 'unsupported official dsh version 0.2.1-rc.1' "$root/startup.txt"
check "startup exit is non-zero" test "$startup_status" -ne 0

echo "=== uninstall (default) ==="
"$payload/uninstall.sh" --json > "$root/uninstall.json"
cat "$root/uninstall.json"
check "entrypoint removed" test ! -e "$NANO_BIN_DIR/nano"
check "install root removed" test ! -e "$NANO_INSTALL_ROOT"
check "data root retained" test -d "$NANO_DATA_ROOT"
check "profile retained by default" test -d "$DSH_HOME/profiles/dsh-tui-nano"
check "config and credentials retained" test "$(snapshot)" = "$before"

echo "=== uninstall (explicit profile and data removal) ==="
"$payload/uninstall.sh" --remove-profile --purge-data --json > "$root/uninstall-purge.json"
cat "$root/uninstall-purge.json"
check "profile removed explicitly" test ! -e "$DSH_HOME/profiles/dsh-tui-nano"
check "data root removed explicitly" test ! -e "$NANO_DATA_ROOT"
check "other profile still retained" grep -q '"name":"other-product"' "$DSH_HOME/profiles/other-product/package.json"
check "config and credentials still retained" test "$(snapshot)" = "$before"

if [ "$fail" -ne 0 ]; then
  echo "RESULT fail"
  exit 1
fi
echo "RESULT ok"
