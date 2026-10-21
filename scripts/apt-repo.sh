#!/usr/bin/env bash
# Makes the apt repository for a release: a signed "flat" repository (Packages, Release, InRelease,
# Release.gpg) next to the .debs from scripts/build-deb.sh, all uploaded as assets of the GitHub
# release. apt reads it from releases/latest/download, so every new release is an update:
#   deb [signed-by=/usr/share/keyrings/banana-photos.gpg] https://github.com/aqibshabbirdev/banana-rs/releases/latest/download ./
# Signs with the "Banana Photos apt repository" GPG key (override with APT_KEY=<fingerprint>).
# Output: dist/apt/ (upload everything in it to the release).
set -euo pipefail
cd "$(dirname "$0")/.."

KEY="${APT_KEY:-EE4D8CFC75089E9F47640DBB75173A027F9AC63E}"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
OUT=dist/apt
rm -rf "$OUT"
mkdir -p "$OUT"
cp dist/deb/banana-photos_"$VERSION"_*.deb "$OUT"/

docker run --rm -v "$PWD/$OUT":/repo -w /repo ubuntu:24.04 bash -euc '
  apt-get update -qq >/dev/null && apt-get install -y -qq apt-utils >/dev/null
  # Flat repository: file names without the leading "./", so apt asks for <base>/<file>.
  apt-ftparchive packages . | sed "s|^Filename: \./|Filename: |" > Packages
  gzip -9kn Packages
  apt-ftparchive -o APT::FTPArchive::Release::Origin="Banana Photos" \
    -o APT::FTPArchive::Release::Label="Banana Photos" \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Description="Banana Photos, an offline photo library" \
    release . > /tmp/Release && mv /tmp/Release Release
'
gpg --batch --yes -u "$KEY" --clearsign -o "$OUT/InRelease" "$OUT/Release"
gpg --batch --yes -u "$KEY" -abs -o "$OUT/Release.gpg" "$OUT/Release"
gpg --export "$KEY" > "$OUT/banana-photos.gpg"
echo "Built $OUT:"
ls -1 "$OUT"
