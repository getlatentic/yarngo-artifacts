#!/usr/bin/env bash
# Package Yarngo Studio.
#
# cargo-packager reads [package.metadata.packager] and emits the right artifact
# per host: .app/.dmg on macOS, NSIS/MSI on Windows, deb/AppImage on Linux.
# It replaces the hand-rolled bundle script — cargo-bundle has no Windows
# support, and Zed's approach (patched cargo-bundle plus Inno Setup) is a
# workaround from before this tooling existed.
#
# macOS note: microphone access needs the bundle identity, not a paid
# certificate. cargo-packager ad-hoc signs by default, which is enough for TCC
# to keep the grant stable across rebuilds. A Developer ID and notarization are
# only needed so *other people* can open it without Gatekeeper complaining.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo-packager >/dev/null || {
  echo "installing cargo-packager…"
  cargo install cargo-packager --locked
}

PROFILE="${PROFILE:-debug}"
# cargo's dev profile writes to target/debug; cargo-packager names the directory
# directly, so the two need translating rather than passing through.
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p voicestudio
  cargo packager --release ${FORMATS:+--formats "$FORMATS"} "$@"
else
  cargo build -p voicestudio
  cargo packager ${FORMATS:+--formats "$FORMATS"} "$@"
fi

APP="target/$PROFILE/Yarngo Studio.app"
[[ -d "$APP" ]] || { echo "no bundle at $APP" >&2; exit 1; }

# cargo-packager ad-hoc signs without entitlements and derives an identifier
# from the binary name. Both matter here: without the audio-input entitlement
# recording silently produces nothing, and a derived identifier changes between
# builds so macOS treats each build as a new app and re-asks for the microphone.
# Re-sign explicitly, with a real identity when one is configured.
IDENTITY="${APPLE_SIGNING_IDENTITY:--}"
sign() {
  codesign --force --timestamp --options runtime \
    --entitlements packaging/yarngo-studio.entitlements \
    --identifier dev.yarngo.studio \
    --sign "$IDENTITY" "$1"
}

# Inner executables before the enclosing bundle.
while IFS= read -r bin; do sign "$bin"; done < <(
  find "$APP/Contents/Resources" -type f -perm +111 2>/dev/null
)
sign "$APP/Contents/MacOS/voicestudio"
sign "$APP"

codesign --verify --strict --verbose=2 "$APP" 2>&1 | tail -2
echo "packaged: $APP"

# --- Notarization, when a Developer ID is configured -------------------------
# Learned from Zed's bundle-mac: the order and flags matter, and getting them
# wrong surfaces late, at submission time.
#
#   * --options runtime   hardened runtime; notarization refuses without it
#   * --timestamp         secure timestamp; also required
#   * inner binaries are signed BEFORE the enclosing .app, never after
#   * entitlements are applied to the executable AND the bundle
#   * notarytool submit --wait, then stapler staple, so the artifact validates
#     offline afterwards
#
# Set APPLE_SIGNING_IDENTITY to enable. Without it cargo-packager ad-hoc signs,
# which is all local microphone access needs.
notarize() {
  local app="$1"
  [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] || { echo "no APPLE_SIGNING_IDENTITY; left ad-hoc signed"; return 0; }

  # Sidecar first, enclosing bundle last.
  find "$app/Contents/Resources/sidecar" -type f -perm +111 2>/dev/null | while read -r bin; do
    codesign --force --timestamp --options runtime \
      --entitlements packaging/yarngo-studio.entitlements \
      --sign "$APPLE_SIGNING_IDENTITY" "$bin"
  done
  codesign --force --timestamp --options runtime \
    --entitlements packaging/yarngo-studio.entitlements \
    --sign "$APPLE_SIGNING_IDENTITY" "$app/Contents/MacOS/voicestudio"
  codesign --force --timestamp --options runtime \
    --entitlements packaging/yarngo-studio.entitlements \
    --sign "$APPLE_SIGNING_IDENTITY" "$app"

  codesign --verify --deep --strict --verbose=2 "$app"
}
