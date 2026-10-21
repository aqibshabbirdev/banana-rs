#!/usr/bin/env bash
# Uploads the App Store build to App Store Connect through Xcode's signed-in account (no password
# prompt): wraps dist/Banana Photos.app (from APPSTORE=1 scripts/build-macos.sh) in an .xcarchive and
# runs xcodebuild -exportArchive with destination "upload". The version must match the version page
# in App Store Connect, and CFBundleVersion must be higher than any build uploaded before.
#   APPSTORE=1 BUILD_NUMBER=3 scripts/build-macos.sh && scripts/upload-appstore.sh
set -euo pipefail
cd "$(dirname "$0")/.."
APP="dist/Banana Photos.app"
TEAM_ID="${TEAM_ID:-4KV8W3FP9B}"
[ -f "$APP/Contents/embedded.provisionprofile" ] || { echo "Build it first: APPSTORE=1 scripts/build-macos.sh" >&2; exit 1; }
info() { /usr/libexec/PlistBuddy -c "Print :$1" "$APP/Contents/Info.plist"; }
VERSION=$(info CFBundleShortVersionString)
BUILD=$(info CFBundleVersion)
BUNDLE_ID=$(info CFBundleIdentifier)

WORK="$(mktemp -d)"
ARCHIVE="$WORK/BananaPhotos.xcarchive"
mkdir -p "$ARCHIVE/Products/Applications"
ditto "$APP" "$ARCHIVE/Products/Applications/Banana Photos.app"
cat > "$ARCHIVE/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>ApplicationProperties</key>
  <dict>
    <key>ApplicationPath</key><string>Applications/Banana Photos.app</string>
    <key>Architectures</key><array><string>arm64</string></array>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$BUILD</string>
    <key>Team</key><string>$TEAM_ID</string>
  </dict>
  <key>ArchiveVersion</key><integer>2</integer>
  <key>CreationDate</key><date>$(date -u +%Y-%m-%dT%H:%M:%SZ)</date>
  <key>Name</key><string>Banana Photos</string>
  <key>SchemeName</key><string>Banana Photos</string>
</dict>
</plist>
PLIST
cat > "$WORK/export.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>${DESTINATION:-upload}</string>
  <key>signingStyle</key><string>automatic</string>
  <key>teamID</key><string>$TEAM_ID</string>
  <key>uploadSymbols</key><false/>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict>
</plist>
PLIST
echo "==> Uploading Banana Photos $VERSION ($BUILD)"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$WORK/out" \
  -exportOptionsPlist "$WORK/export.plist" -allowProvisioningUpdates 2>&1 |
  grep -vE "not yet complete|Checking completion|^\s*$" | tail -5
rm -rf "$WORK"
