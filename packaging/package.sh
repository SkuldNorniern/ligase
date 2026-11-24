#!/usr/bin/env bash
# packs target/release/ligase for the os it runs on, into the current folder:
# a zip and the kago setup on windows, a dmg on macos, a tarball on linux,
# each with a .sha256 next to it.
#
#   packaging/package.sh <label> <triple>     # label: v0.1.0, nightly-2026-11-19
set -euo pipefail

label=$1
triple=$2
name="ligase-$label-$triple"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
icons=assets/icons/app

# pdfium for pdf pages, prebuilt by bblanchon/pdfium-binaries (bsd-3 /
# apache-2.0, no gpl). pinned; ligase loads it only when a pdf opens.
pdfium_tag=chromium/8179
pdfium() {
  local asset
  case "$triple" in
    x86_64-pc-windows-*) asset=pdfium-win-x64.tgz ;;
    aarch64-pc-windows-*) asset=pdfium-win-arm64.tgz ;;
    aarch64-apple-*) asset=pdfium-mac-arm64.tgz ;;
    x86_64-apple-*) asset=pdfium-mac-x64.tgz ;;
    aarch64-unknown-linux-*) asset=pdfium-linux-arm64.tgz ;;
    *) asset=pdfium-linux-x64.tgz ;;
  esac
  rm -rf third_party/pdfium && mkdir -p third_party/pdfium
  curl -fsSL -o third_party/pdfium.tgz \
    "https://github.com/bblanchon/pdfium-binaries/releases/download/$pdfium_tag/$asset"
  tar -xzf third_party/pdfium.tgz -C third_party/pdfium
  rm third_party/pdfium.tgz
}
pdfium

sum() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" > "$1.sha256"
  else
    shasum -a 256 "$1" > "$1.sha256"
  fi
}

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    rm -rf pack && mkdir pack
    cp target/release/ligase.exe pack/
    cp third_party/pdfium/bin/pdfium.dll pack/
    mkdir -p pack/licenses/pdfium
    cp -R third_party/pdfium/LICENSE third_party/pdfium/licenses/. pack/licenses/pdfium/
    (cd pack && 7z a "../$name.zip" ligase.exe pdfium.dll licenses >/dev/null)
    sum "$name.zip"
    # the setup; kago names it Ligase-<version>-setup-windows-<arch>.exe
    kago build --no-build --out setup
    for f in setup/*.exe; do
      mv "$f" "$name-setup.exe"
      sum "$name-setup.exe"
    done
    if [[ -f setup/latest.json ]]; then
      sed "s/Ligase-$version-setup-windows-x86_64.exe/$name-setup.exe/" setup/latest.json > latest.json
    fi
    ;;
  Darwin)
    rm -rf Ligase.iconset Ligase.app dmg
    mkdir Ligase.iconset
    for s in 16 32 128 256 512; do
      sips -z $s $s "$icons/ligase-mac-1024.png" --out "Ligase.iconset/icon_${s}x${s}.png" >/dev/null
      d=$((s * 2))
      sips -z $d $d "$icons/ligase-mac-1024.png" --out "Ligase.iconset/icon_${s}x${s}@2x.png" >/dev/null
    done
    mkdir -p Ligase.app/Contents/MacOS Ligase.app/Contents/Resources
    iconutil -c icns Ligase.iconset -o Ligase.app/Contents/Resources/ligase.icns
    cp target/release/ligase Ligase.app/Contents/MacOS/ligase
    chmod +x Ligase.app/Contents/MacOS/ligase
    mkdir -p Ligase.app/Contents/Frameworks Ligase.app/Contents/Resources/licenses/pdfium
    cp third_party/pdfium/lib/libpdfium.dylib Ligase.app/Contents/Frameworks/
    cp -R third_party/pdfium/LICENSE third_party/pdfium/licenses/. Ligase.app/Contents/Resources/licenses/pdfium/
    plist=Ligase.app/Contents/Info.plist
    cp packaging/macos/Info.plist "$plist"
    plutil -replace CFBundleShortVersionString -string "$version" "$plist"
    plutil -replace CFBundleVersion -string "$version" "$plist"
    plutil -lint "$plist"
    codesign --force --deep --sign - Ligase.app
    codesign --verify --deep --strict --verbose=2 Ligase.app
    # the dmg opens on the app and a link to drag it into
    mkdir dmg
    cp -R Ligase.app dmg/
    ln -s /Applications dmg/Applications
    hdiutil create -volname Ligase -srcfolder dmg -ov -format UDZO "$name.dmg"
    sum "$name.dmg"
    ;;
  *)
    rm -rf pack && mkdir -p pack/bin pack/share/applications
    cp target/release/ligase pack/bin/
    cp third_party/pdfium/lib/libpdfium.so pack/bin/
    mkdir -p pack/share/licenses/ligase/pdfium
    cp -R third_party/pdfium/LICENSE third_party/pdfium/licenses/. pack/share/licenses/ligase/pdfium/
    cp packaging/linux/ligase.desktop pack/share/applications/
    for s in 16 24 32 48 64 128 256 512; do
      mkdir -p "pack/share/icons/hicolor/${s}x${s}/apps"
      cp "$icons/ligase-$s.png" "pack/share/icons/hicolor/${s}x${s}/apps/ligase.png"
    done
    tar -czf "$name.tar.gz" -C pack .
    sum "$name.tar.gz"
    ;;
esac
