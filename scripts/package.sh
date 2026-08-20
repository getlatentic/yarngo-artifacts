#!/usr/bin/env bash
# Package yarngo studio.
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

# `package.sh release` is the obvious thing to type, so accept it rather than
# forwarding it to cargo-packager as an unknown subcommand. Anything else
# positional is a mistake worth naming here instead of failing further down.
if [[ "${1:-}" == "release" || "${1:-}" == "debug" ]]; then
  PROFILE="$1"; shift
fi
if [[ $# -gt 0 ]]; then
  echo "unexpected argument: $1 (usage: package.sh [release|debug])" >&2
  exit 2
fi

PROFILE="${PROFILE:-debug}"
APP="target/$PROFILE/yarngo studio.app"

# The .app is built and signed BEFORE the disk image is made, so the image
# wraps a signed app rather than the ad-hoc one cargo-packager leaves behind.
# Built the other way round, the artifact people actually download carries the
# wrong signature — and a failure making the image used to abort the run before
# signing happened at all, which is the same bug wearing a different hat.
pack() {
  # cargo's dev profile writes to target/debug; cargo-packager names the
  # directory directly, so the two need translating rather than passing through.
  if [[ "$PROFILE" == "release" ]]; then
    cargo packager --release --formats "$1" "${@:2}"
  else
    cargo packager --formats "$1" "${@:2}"
  fi
}

# Running the sidecar leaves __pycache__ beside it, which the resource glob
# matches and the packager cannot copy — packaging fails on any machine that has
# actually run the app, which is every machine that would package it.
find sidecar -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null || true

if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p voicestudio
else
  cargo build -p voicestudio
fi

pack app "$@"
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

# Now the image, around the signed app — built here rather than by
# cargo-packager, because `--formats dmg` re-creates the .app as a dependency
# and would replace the bundle that was just signed with a fresh ad-hoc one.
# The image people download has to contain the signature, not a copy of it made
# a moment too early.
if [[ "${FORMATS:-dmg}" == *dmg* ]]; then
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  DMG="target/$PROFILE/yarngo studio_${VERSION}_aarch64.dmg"
  STAGE=$(mktemp -d)

  # A run that failed part way can leave its volume mounted, and every later
  # attempt then fails on a name already taken. Clear it rather than reporting
  # it as a new fault.
  while IFS= read -r stale; do
    [[ -n "$stale" ]] && hdiutil detach "$stale" -force >/dev/null 2>&1 || true
  done < <(ls -d "/Volumes/yarngo studio"* 2>/dev/null || true)

  cp -R "$APP" "$STAGE/"
  # Somewhere to drop it, so the image explains itself without instructions.
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG"
  if hdiutil create -volname "yarngo studio" -srcfolder "$STAGE" \
      -ov -format UDZO "$DMG" >/dev/null; then
    [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] &&
      codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$DMG"
    echo "packaged: $DMG"
  else
    # A disk image is a convenience; the signed app is the artifact that
    # matters, and losing the run over the wrapper would be the wrong trade.
    echo "the disk image could not be built; the signed app stands" >&2
  fi
  rm -rf "$STAGE"
fi

# --- Notarization, when a Developer ID is configured -------------------------
# Ad-hoc signing is enough for this machine: microphone access needs the bundle
# identity, not a certificate. It is not enough for anyone else's — Gatekeeper
# refuses an app it cannot trace to a Developer ID, so a build that is only
# ad-hoc signed cannot be handed to another person at all.
#
# Order and flags matter, and getting them wrong surfaces late, at submission:
#
#   * --options runtime   hardened runtime; notarization refuses without it
#   * --timestamp         secure timestamp; also required
#   * inner binaries are signed BEFORE the enclosing .app, never after
#   * entitlements are applied to the executable AND the bundle
#   * notarytool submit --wait, then stapler staple, so the artifact validates
#     offline afterwards
#
# Credentials come from either a stored keychain profile:
#
#   xcrun notarytool store-credentials yarngo --apple-id … --team-id … --password …
#   APPLE_SIGNING_IDENTITY="Developer ID Application: …" APPLE_NOTARY_PROFILE=yarngo ./scripts/package.sh
#
# or from the three values directly, as APPLE_ID, APPLE_TEAM_ID and
# APPLE_PASSWORD (an app-specific password, not the account one).

notary_args() {
  if [[ -n "${APPLE_NOTARY_PROFILE:-}" ]]; then
    printf '%s\0%s\0' --keychain-profile "$APPLE_NOTARY_PROFILE"
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_PASSWORD:-}" ]]; then
    printf '%s\0%s\0%s\0%s\0%s\0%s\0' \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD"
  fi
}

notarize() {
  local artifact="$1"
  [[ -f "$artifact" || -d "$artifact" ]] || return 0

  local -a creds=()
  while IFS= read -r -d "" arg; do creds+=("$arg"); done < <(notary_args)
  if [[ ${#creds[@]} -eq 0 ]]; then
    echo "no notary credentials; $artifact is signed but NOT notarized" >&2
    return 0
  fi

  echo "notarizing $artifact …"
  xcrun notarytool submit "$artifact" "${creds[@]}" --wait || {
    echo "notarization failed for $artifact" >&2
    return 1
  }
  # Staple so the artifact validates without a network round-trip on first open.
  xcrun stapler staple "$artifact"
  xcrun stapler validate "$artifact"
}

if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  cat >&2 <<'NOTE'

  This build is ad-hoc signed. It runs here, and Gatekeeper will refuse it on
  any other Mac. Set APPLE_SIGNING_IDENTITY to a Developer ID Application
  certificate, plus notary credentials, to produce something distributable.

NOTE
else
  # The .app has to be notarized inside a container; the .dmg is what people
  # download, so notarize and staple that, and staple the .app too so a bare
  # copy of it also validates.
  notarize "$APP"
  DMG=$(ls -t "target/$PROFILE"/*.dmg 2>/dev/null | head -1 || true)
  [[ -n "$DMG" ]] && notarize "$DMG"
fi

# --- What a released build must satisfy --------------------------------------
# Checked here rather than trusted, because every one of these fails silently:
# an unsigned inner binary only surfaces at submission, and a missing
# entitlement only surfaces as a microphone that records nothing.
if [[ "$PROFILE" == "release" ]]; then
  echo "--- release checks ---"
  codesign --verify --deep --strict --verbose=2 "$APP"
  codesign -d --entitlements - "$APP" 2>/dev/null | grep -q "audio-input" \
    || { echo "the audio-input entitlement is missing" >&2; exit 1; }
  [[ -f "$APP/Contents/Resources/YarngoStudio.icns" ]] \
    || { echo "the app icon is missing" >&2; exit 1; }
  if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
    spctl --assess --type execute --verbose=4 "$APP" \
      || { echo "Gatekeeper would refuse this build" >&2; exit 1; }
  fi
  echo "release checks passed"
fi
