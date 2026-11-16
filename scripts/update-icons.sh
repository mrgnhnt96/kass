#!/bin/bash
set -e

# Regenerates every Kass icon from the Icon Composer source, tauri/assets/kass.icon.
#
#   1. Renders the Liquid Glass appearances (Default, Dark, Clear, Tinted) with ictool
#   2. Compiles kass.icon with actool into Assets.car + kass.icns
#   3. Writes the Tauri fallback PNGs and icon.icns
#   4. Writes the in-app logo, the site icons and the README icon
#
# The favicons (site/public/favicon.svg, app/public/favicon.svg) are hand-drawn SVGs with
# thicker strokes so the mark holds up at 16 px; edit those directly.
#
# Requires Xcode 26+ (Icon Composer's ictool), ImageMagick (for the README webp) and
# site/node_modules (sharp, for the apple-touch icon).

cd "$(dirname "$0")/.."

ICTOOL="$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool"
ICON_BUNDLE="tauri/assets/kass.icon"
EXPORTS_DIR="tauri/assets/kass_exports"
ICONS_DIR="tauri/src-tauri/icons"
GEN_DIR="tauri/src-tauri/gen"
SOURCE_ICON="$EXPORTS_DIR/kass-macOS-Default-1024x1024@1x.png"

if [ ! -x "$ICTOOL" ]; then
  echo "Error: ictool not found at $ICTOOL (install Xcode 26 or later)"
  exit 1
fi

render() { # platform rendition size output
  "$ICTOOL" "$ICON_BUNDLE" --export-image --output-file "$4" \
    --platform "$1" --rendition "$2" --width "$3" --height "$3" --scale 1 >/dev/null
}

echo "🎨 Updating all Kass icons from $ICON_BUNDLE"

# 1. Appearance renders
echo "Rendering appearances..."
mkdir -p "$EXPORTS_DIR"
rm -f "$EXPORTS_DIR"/*.png
for rendition in Default Dark ClearLight ClearDark TintedLight TintedDark; do
  render macOS "$rendition" 1024 "$EXPORTS_DIR/kass-macOS-$rendition-1024x1024@1x.png"
done

# 2. Liquid Glass compile (same invocation as tauri/src-tauri/build.rs)
echo "Compiling kass.icon with actool..."
mkdir -p "$GEN_DIR"
rm -f "$GEN_DIR/kass.icns" "$GEN_DIR/Assets.car" "$GEN_DIR/partial.plist"
xcrun actool --compile "$GEN_DIR" --output-format human-readable-text \
  --output-partial-info-plist "$GEN_DIR/partial.plist" --app-icon kass \
  --include-all-app-icons --target-device mac --minimum-deployment-target 11.0 \
  --platform macosx "$ICON_BUNDLE" >/dev/null

# 3. Tauri fallback icons
echo "Writing Tauri icons..."
mkdir -p "$ICONS_DIR"
sips -s format png -z 32 32 "$SOURCE_ICON" --out "$ICONS_DIR/32x32.png" >/dev/null
sips -s format png -z 64 64 "$SOURCE_ICON" --out "$ICONS_DIR/64x64.png" >/dev/null
sips -s format png -z 128 128 "$SOURCE_ICON" --out "$ICONS_DIR/128x128.png" >/dev/null
sips -s format png -z 256 256 "$SOURCE_ICON" --out "$ICONS_DIR/128x128@2x.png" >/dev/null
sips -s format png -z 512 512 "$SOURCE_ICON" --out "$ICONS_DIR/icon.png" >/dev/null
cp "$GEN_DIR/kass.icns" "$ICONS_DIR/icon.icns"
# build.rs falls back to this PNG when actool cannot produce kass.icns
cp "$SOURCE_ICON" "$ICON_BUNDLE/Assets/Kass.png"

# 4. App, site and README
echo "Writing app, site and README icons..."
cp "$SOURCE_ICON" tauri/assets/kass-logo.png
sips -s format png -z 512 512 "$SOURCE_ICON" --out site/public/icon.png >/dev/null
sips -s format png -z 256 256 "$SOURCE_ICON" --out site/src/assets/icon.png >/dev/null
# iOS masks the home-screen icon itself, so it must be a full-bleed square
(cd site && node -e "require('sharp')(Buffer.from(process.argv[1])).png().toFile('public/apple-touch-icon.png')" \
  '<svg xmlns="http://www.w3.org/2000/svg" width="180" height="180" viewBox="0 0 100 100"><rect width="100" height="100" fill="#111318"/><g fill="none" stroke-width="8" stroke-linecap="round"><path d="M18 58 Q 34 55, 54 32" stroke="#F4F2EC"/><path d="M22 74 Q 48 70, 80 26" stroke="#5CF2B0"/><path d="M36 82 Q 60 78, 82 50" stroke="#F4F2EC"/></g></svg>')
magick "$EXPORTS_DIR/kass-macOS-Dark-1024x1024@1x.png" -resize 256x256 docs/assets/icon-dark.webp

echo "✅ Icons updated. Rebuild the app to pick them up."
