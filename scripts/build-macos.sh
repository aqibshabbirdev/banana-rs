#!/usr/bin/env bash
# Builds "Banana Photos.app" for Apple Silicon (M1–M4) and wraps it in a .dmg.
#   scripts/build-macos.sh            -> dist/Banana Photos.app, dist/BananaPhotos-<version>-arm64.dmg
#   SIGN_ID="Developer ID Application: ..." scripts/build-macos.sh   to sign for distribution
# Needs only Xcode command line tools and Rust; HEIC uses macOS's ImageIO, so nothing to bundle.
set -euo pipefail

cd "$(dirname "$0")/.."
APP_NAME="Banana Photos"
BUNDLE_ID="com.bananars.photos"
TARGET="aarch64-apple-darwin"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
DIST="dist"
APP="$DIST/$APP_NAME.app"
SIGN_ID="${SIGN_ID:--}"   # "-" = ad-hoc, enough to run on this Mac and on others after "Open"

export MACOSX_DEPLOYMENT_TARGET=11.0
rustup target add "$TARGET" >/dev/null 2>&1 || true

echo "==> Building release binary ($TARGET)"
cargo build --release --target "$TARGET"

if [ ! -f assets/icon.png ]; then
  cargo run --release --example make_icon -- assets/icon.png
fi

echo "==> Fetching People & pets models"
scripts/fetch-models.sh assets/models

echo "==> Making icon"
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/banana-rs" "$APP/Contents/MacOS/banana-rs"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
cp -R assets/models "$APP/Contents/Resources/models"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>banana-rs</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
  <key>LSArchitecturePriority</key><array><string>arm64</string></array>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSPhotoLibraryUsageDescription</key><string>Banana Photos copies the photos you choose from the Photos app into your library folder.</string>
  <key>NSDesktopFolderUsageDescription</key><string>Banana Photos shows the photos in folders you open.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>Banana Photos shows the photos in folders you open.</string>
  <key>NSDownloadsFolderUsageDescription</key><string>Banana Photos shows the photos in folders you open.</string>
  <key>NSRemovableVolumesUsageDescription</key><string>Banana Photos shows photos from drives and SD cards you open.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>Banana Photos shows photos from network folders you open.</string>
</dict>
</plist>
PLIST

echo "==> Signing ($SIGN_ID)"
if [ "$SIGN_ID" = "-" ]; then
  codesign --force --deep --sign - "$APP"
else
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_ID" "$APP"
fi
codesign --verify --strict "$APP"

echo "==> Making dmg"
DMG="$DIST/BananaPhotos-$VERSION-arm64.dmg"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo
echo "Done:"
echo "  $APP"
echo "  $DMG ($(du -h "$DMG" | cut -f1))"
