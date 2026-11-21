#!/bin/bash
# Compila Unricopie para macOS y arma Unricopie.app con un binario universal: el mismo programa
# corre en los Mac con Intel y en los de chip M. Se corre en un Mac (Intel o M), desde cualquier
# carpeta:
#
#   bash scripts/mac/compilar.sh
#
# Necesita las herramientas de línea de órdenes de Xcode (`xcode-select --install`) y Rust
# (https://rustup.rs). El catálogo y las recetas de arranque se bajan de recompendium.com.
# Deja paquetes/mac/Unricopie.app y paquetes/Unricopie-<versión>-macos.zip.
set -euo pipefail
cd "$(dirname "$0")/../.."
RAIZ="$(pwd)"

xcode-select -p > /dev/null 2>&1 || { echo "Faltan las herramientas de Xcode: corre «xcode-select --install» y vuelve a probar."; exit 1; }
command -v cargo > /dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo" ] || { echo "Falta Rust: instálalo desde https://rustup.rs y vuelve a probar."; exit 1; }
export PATH="$HOME/.cargo/bin:$PATH"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)"
echo "Unricopie $VERSION para macOS"

# Las dos arquitecturas. Sin las rutas de esta máquina dentro del binario.
rustup target add x86_64-apple-darwin aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=11.0
export RUSTFLAGS="--remap-path-prefix=$HOME/.cargo/registry/src=cargo --remap-path-prefix=$RAIZ=unricopie"
for t in x86_64-apple-darwin aarch64-apple-darwin; do
    echo "== compilando para $t"
    cargo build --release -p unricopie --target "$t"
done

APP="$RAIZ/paquetes/mac/Unricopie.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS/datos" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/unricopie" \
    target/x86_64-apple-darwin/release/unricopie target/aarch64-apple-darwin/release/unricopie
lipo -info "$APP/Contents/MacOS/unricopie"

# El catálogo y las recetas con los que arranca la primera vez, junto al programa (datos.rs).
curl -fsSL https://recompendium.com/unricopie/catalogo.json -o "$APP/Contents/MacOS/datos/catalogo.json"
curl -fsSL https://recompendium.com/unricopie/recetas.json -o "$APP/Contents/MacOS/datos/recetas.json"

# El icono: el pastel, en los tamaños que pide macOS.
ICONOS="$(mktemp -d)/unricopie.iconset"
mkdir -p "$ICONOS"
for n in 16 32 128 256; do
    sips -z $n $n scripts/mac/unricopie.png --out "$ICONOS/icon_${n}x${n}.png" > /dev/null
    d=$((n * 2)); [ $d -le 256 ] && sips -z $d $d scripts/mac/unricopie.png --out "$ICONOS/icon_${n}x${n}@2x.png" > /dev/null
done
iconutil -c icns "$ICONOS" -o "$APP/Contents/Resources/unricopie.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Unricopie</string>
    <key>CFBundleDisplayName</key><string>Unricopie</string>
    <key>CFBundleIdentifier</key><string>com.recompendium.unricopie</string>
    <key>CFBundleExecutable</key><string>unricopie</string>
    <key>CFBundleIconFile</key><string>unricopie</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>GPL-3.0-or-later</string>
</dict>
</plist>
PLIST
cp COPYING "$APP/Contents/Resources/COPYING.txt"

# Firma ad hoc (sin cuenta de Apple): los Mac con chip M no abren código sin ninguna firma.
codesign --force --deep --sign - "$APP"
codesign --verify --verbose "$APP"

# Sin rutas de esta máquina dentro.
if strings "$APP/Contents/MacOS/unricopie" | grep -q "$HOME"; then
    echo "OJO: el binario todavía lleva rutas de $HOME"
fi

ZIP="$RAIZ/paquetes/Unricopie-$VERSION-macos.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo
echo "Listo:"
echo "  $APP"
echo "  $ZIP ($(du -h "$ZIP" | cut -f1))"
