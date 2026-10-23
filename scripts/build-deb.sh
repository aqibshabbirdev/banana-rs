#!/usr/bin/env bash
# Packs a Linux build into a .deb: /usr/bin/banana-rs, the models in /usr/share/banana-rs/models,
# a menu entry and an icon. Runs dpkg-shlibdeps inside the banana-linux Docker image (see
# scripts/linux.Dockerfile), so the library dependencies match Ubuntu 24.04 / Debian 13.
#   scripts/build-deb.sh arm64     # from dist/rel/banana-rs-<version>-linux-arm64
#   scripts/build-deb.sh amd64     # from dist/rel/banana-rs-<version>-linux-x86_64
# Output: dist/deb/banana-photos_<version>_<arch>.deb
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="${1:?usage: scripts/build-deb.sh arm64|amd64}"
case "$ARCH" in
  arm64) TRIPLE=arm64; PLATFORM=linux/arm64 ;;
  amd64) TRIPLE=x86_64; PLATFORM=linux/amd64 ;;
  *) echo "arch must be arm64 or amd64" >&2; exit 1 ;;
esac
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
BUILD="dist/rel/banana-rs-$VERSION-linux-$TRIPLE"
[ -x "$BUILD/banana-rs" ] && [ -d "$BUILD/models" ] || { echo "Missing $BUILD (build it with scripts/build-linux.sh first)" >&2; exit 1; }

ROOT="dist/deb/root-$ARCH"
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/usr/share/banana-rs" "$ROOT/usr/share/applications" \
  "$ROOT/usr/share/icons/hicolor/256x256/apps" "$ROOT/usr/share/icons/hicolor/512x512/apps" "$ROOT/usr/share/doc/banana-photos"
install -m 755 "$BUILD/banana-rs" "$ROOT/usr/bin/banana-rs"
cp -R "$BUILD/models" "$ROOT/usr/share/banana-rs/models"
chmod 644 "$ROOT/usr/share/banana-rs/models/"*
sips -z 256 256 assets/icon.png --out "$ROOT/usr/share/icons/hicolor/256x256/apps/banana-photos.png" >/dev/null
sips -z 512 512 assets/icon.png --out "$ROOT/usr/share/icons/hicolor/512x512/apps/banana-photos.png" >/dev/null
cp LICENSE "$ROOT/usr/share/doc/banana-photos/copyright"

cat > "$ROOT/usr/share/applications/banana-photos.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Banana Photos
GenericName=Photo Library
Comment=Browse your photos by date, offline
Exec=banana-rs %f
Icon=banana-photos
Terminal=false
Categories=Graphics;Photography;Viewer;
Keywords=photos;pictures;gallery;library;
StartupWMClass=banana-rs
EOF

IMAGE="banana-linux-$ARCH"
docker build --platform "$PLATFORM" -q -t "$IMAGE" -f scripts/linux.Dockerfile scripts >/dev/null
docker run --rm --platform "$PLATFORM" -v "$PWD":/src -w /src "$IMAGE" bash -euc '
  ROOT='"$ROOT"'
  # dpkg-shlibdeps wants a debian/control next to it.
  T=$(mktemp -d); mkdir -p $T/debian; printf "Source: banana-photos\n\nPackage: banana-photos\nArchitecture: any\n" > $T/debian/control
  cp $ROOT/usr/bin/banana-rs $T/banana-rs
  (cd $T && dpkg-shlibdeps -O banana-rs 2>/dev/null) | sed -n "s/^shlibs:Depends=//p" > /tmp/deps
  # Plugins are loaded at run time, so dpkg-shlibdeps cannot see them.
  echo "$(cat /tmp/deps), libheif-plugin-libde265, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, libgl1, libegl1, libgl1-mesa-dri, libxkbcommon-x11-0" > $ROOT/../deps-'"$ARCH"'
'
DEPENDS=$(cat "dist/deb/deps-$ARCH")
SIZE=$(du -sk "$ROOT/usr" | cut -f1)

cat > "$ROOT/DEBIAN/control" <<EOF
Package: banana-photos
Version: $VERSION
Architecture: $ARCH
Maintainer: Aqib Shabbir <aqibshabbirdev@users.noreply.github.com>
Installed-Size: $SIZE
Depends: $DEPENDS
Recommends: ffmpeg
Section: graphics
Priority: optional
Homepage: https://github.com/aqibshabbirdev/banana-rs
Description: Offline photo library, laid out like Google Photos
 Banana Photos shows every photo and video in a folder by month and day,
 with memories, albums, favourites, people & pets, documents, an encrypted
 Locked Folder, and search by what's in the photos. Everything runs on your
 computer and nothing is uploaded. Run it as banana-rs or from the menu.
EOF

docker run --rm --platform "$PLATFORM" -v "$PWD":/src -w /src "$IMAGE" \
  dpkg-deb --root-owner-group -Zxz --build "$ROOT" "dist/deb/banana-photos_${VERSION}_$ARCH.deb" >/dev/null
rm -rf "$ROOT" "dist/deb/deps-$ARCH"
echo "Built dist/deb/banana-photos_${VERSION}_$ARCH.deb"
