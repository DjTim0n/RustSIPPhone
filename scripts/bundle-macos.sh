#!/usr/bin/env bash
# Builds the macOS app RustSIPPhone.app (and optionally a .dmg).
#
#   scripts/bundle-macos.sh              # build for the current architecture
#   scripts/bundle-macos.sh --universal  # Intel + Apple Silicon in one app
#   scripts/bundle-macos.sh --dmg        # also pack it into a .dmg
#
# Signing: ad-hoc by default (free, enough to run on your own Mac).
# To distribute widely, set SIGN_IDENTITY="Developer ID Application: ...".
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="RustSIPPhone"
DISPLAY_NAME="RustSIPPhone"
BUNDLE_ID="com.rustsipphone.app"
BIN_NAME="rust_sip_phone"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"

UNIVERSAL=0
MAKE_DMG=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    --dmg) MAKE_DMG=1 ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

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
codesign --force --deep --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" "$APP"
codesign --verify --deep --strict "$APP"

echo "Done: $APP"

if [ "$MAKE_DMG" -eq 1 ]; then
  echo "==> DMG"
  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  DMG="$DIST/$APP_NAME-$VERSION.dmg"
  rm -f "$DMG"
  hdiutil create -volname "$DISPLAY_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
  echo "Done: $DMG"
fi
