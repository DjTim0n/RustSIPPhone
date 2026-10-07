#!/usr/bin/env bash
# Builds the macOS app RustSIPPhone.app (and optionally a .dmg).
#
#   scripts/bundle-macos.sh              # build for the current architecture
#   scripts/bundle-macos.sh --universal  # Intel + Apple Silicon in one app
#   scripts/bundle-macos.sh --dmg        # also pack it into a .dmg
#   scripts/bundle-macos.sh --notarize   # sign with Developer ID, notarize with Apple, staple
#
# Signing
#   Ad-hoc by default: free, enough to run on your own Mac, but other Macs show a
#   "could not verify" warning. Set SIGN_IDENTITY="Developer ID Application: ..." to sign with
#   your certificate (hardened runtime + microphone entitlement). --notarize finds the
#   certificate in the keychain by itself.
#
# Notarization (--notarize) needs credentials, one of:
#   1. A saved profile (default name "rustsipphone-notary", or set NOTARY_PROFILE). Create it once:
#        xcrun notarytool store-credentials "rustsipphone-notary" \
#          --apple-id YOU@EXAMPLE.COM --team-id TEAMID --password APP-SPECIFIC-PASSWORD
#   2. APPLE_API_KEY_PATH, APPLE_API_KEY_ID and APPLE_API_ISSUER (App Store Connect API key).
#   3. APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD (what CI uses).
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="RustSIPPhone"
DISPLAY_NAME="RustSIPPhone"
BUNDLE_ID="com.rustsipphone.app"
BIN_NAME="rust_sip_phone"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"

ENTITLEMENTS="packaging/macos/entitlements.plist"

UNIVERSAL=0
MAKE_DMG=0
NOTARIZE=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    --dmg) MAKE_DMG=1 ;;
    --notarize) NOTARIZE=1 ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

NOTARY_ARGS=()
if [ "$NOTARIZE" -eq 1 ]; then
  # Use the Developer ID certificate from the keychain unless one was given explicitly.
  if [ "$SIGN_IDENTITY" = "-" ]; then
    SIGN_IDENTITY="$(security find-identity -v -p codesigning \
      | sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' | head -1)"
    if [ -z "$SIGN_IDENTITY" ]; then
      echo "No 'Developer ID Application' certificate found in the keychain." >&2
      exit 1
    fi
  fi
  case "$SIGN_IDENTITY" in
    "Developer ID Application:"*) ;;
    *) echo "Notarization needs a 'Developer ID Application' identity, got: $SIGN_IDENTITY" >&2; exit 1 ;;
  esac

  if [ -n "${NOTARY_PROFILE:-}" ]; then
    NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
  elif [ -n "${APPLE_API_KEY_PATH:-}" ]; then
    NOTARY_ARGS=(--key "$APPLE_API_KEY_PATH" --key-id "${APPLE_API_KEY_ID:?}" --issuer "${APPLE_API_ISSUER:?}")
  elif [ -n "${APPLE_ID:-}" ]; then
    NOTARY_ARGS=(--apple-id "$APPLE_ID" --team-id "${APPLE_TEAM_ID:?}" --password "${APPLE_APP_PASSWORD:?}")
  else
    NOTARY_ARGS=(--keychain-profile "rustsipphone-notary")
  fi

  # Fail now, not after several minutes of building, if the credentials do not work.
  echo "==> Checking notarization credentials"
  if ! xcrun notarytool history "${NOTARY_ARGS[@]}" >/dev/null 2>&1; then
    echo "Could not log in to the notary service. Save credentials once with:" >&2
    echo '  xcrun notarytool store-credentials "rustsipphone-notary" --apple-id YOU@EXAMPLE.COM --team-id TEAMID --password APP-SPECIFIC-PASSWORD' >&2
    exit 1
  fi
fi

# Sign a path. Ad-hoc signing is enough locally. A real identity gets the hardened runtime,
# a secure timestamp and the microphone entitlement, all of which notarization requires.
sign() {
  if [ "$SIGN_IDENTITY" = "-" ]; then
    codesign --force --deep --sign - --identifier "$BUNDLE_ID" "$1"
  else
    codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" \
      --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" "$1"
  fi
}

# Send a file (.zip or .dmg) to Apple and wait for the verdict; stop with the log if it is refused.
notarize() {
  local file="$1" out status id
  echo "    Submitting $(basename "$file") to Apple, this takes a few minutes..."
  out="$(xcrun notarytool submit "$file" "${NOTARY_ARGS[@]}" --wait --output-format json)" || true
  status="$(printf '%s' "$out" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("status",""))' 2>/dev/null || true)"
  id="$(printf '%s' "$out" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("id",""))' 2>/dev/null || true)"
  if [ "$status" != "Accepted" ]; then
    echo "Notarization failed (status: ${status:-unknown})." >&2
    [ -n "$id" ] && xcrun notarytool log "$id" "${NOTARY_ARGS[@]}" >&2
    exit 1
  fi
  echo "    Accepted by Apple."
}

DIST="dist"
APP="$DIST/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "==> Building (release)"
if [ "$UNIVERSAL" -eq 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  lipo -create \
    "target/aarch64-apple-darwin/release/$BIN_NAME" \
    "target/x86_64-apple-darwin/release/$BIN_NAME" \
    -output "$APP/Contents/MacOS/$BIN_NAME"
else
  cargo build --release
  cp "target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"
fi

echo "==> Icon"
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

echo "==> Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$DISPLAY_NAME</string>
  <key>CFBundleDisplayName</key><string>$DISPLAY_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>$BIN_NAME</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.business</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key>
  <string>The microphone is needed so the other person can hear you during a call.</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"

echo "==> Localized permission text"
# English is the default (Info.plist). Russian users get the prompt in Russian.
mkdir -p "$APP/Contents/Resources/en.lproj" "$APP/Contents/Resources/ru.lproj"
cat > "$APP/Contents/Resources/en.lproj/InfoPlist.strings" <<'STRINGS'
"NSMicrophoneUsageDescription" = "The microphone is needed so the other person can hear you during a call.";
STRINGS
cat > "$APP/Contents/Resources/ru.lproj/InfoPlist.strings" <<'STRINGS'
"NSMicrophoneUsageDescription" = "Микрофон нужен, чтобы собеседник слышал вас во время звонка.";
STRINGS

echo "==> Signing ($SIGN_IDENTITY)"
sign "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

if [ "$NOTARIZE" -eq 1 ]; then
  echo "==> Notarizing the app"
  ZIP="$DIST/$APP_NAME-notarize.zip"
  rm -f "$ZIP"
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  rm -f "$ZIP"
  # Attach Apple's ticket to the app so it opens even without an internet connection.
  xcrun stapler staple "$APP"
  spctl --assess --type exec --verbose=2 "$APP"
fi

echo "Done: $APP"

if [ "$MAKE_DMG" -eq 1 ]; then
  echo "==> DMG"
  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  DMG="$DIST/$APP_NAME-$VERSION.dmg"
  rm -f "$DMG"
  hdiutil create -volname "$DISPLAY_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null

  if [ "$SIGN_IDENTITY" != "-" ]; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
  fi
  if [ "$NOTARIZE" -eq 1 ]; then
    echo "==> Notarizing the disk image"
    notarize "$DMG"
    xcrun stapler staple "$DMG"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
  fi
  echo "Done: $DMG"
fi
