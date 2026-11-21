#!/bin/bash
# Compila Unricopie para Linux y arma sus dos paquetes. Corre dentro del contenedor de
# scripts/linux/Dockerfile; lo llama scripts/empaquetar.py --linux, que monta:
#   /src      el proyecto (solo lectura)
#   /entrada  catalogo.json, recetas.json y README.txt, ya preparados
#   /salida   la carpeta paquetes/
#   /target   la caché de la compilación (un volumen de Docker)
# Uso: empaquetar.sh <versión> [texto que no puede quedar en el binario…]
set -euo pipefail
VERSION="$1"
shift
NOMBRE="Unricopie-$VERSION"
BIN=/target/release/unricopie

echo "== compilando en release (glibc $(ldd --version | head -1 | grep -o '[0-9.]*$')), sin las rutas de la máquina"
# Si varias encajan, rustc aplica la última: de la más general a la más concreta.
export RUSTFLAGS="--remap-path-prefix=/root=~ --remap-path-prefix=$RUSTUP_HOME=rustup --remap-path-prefix=$CARGO_HOME=cargo --remap-path-prefix=/src=unricopie-launcher --remap-path-prefix=/target=target"
cd /src
cargo build --release --locked -p unricopie --target-dir /target

TRABAJO="$(mktemp -d)"
trap 'rm -rf "$TRABAJO"' EXIT
cp "$BIN" "$TRABAJO/unricopie"
strip --strip-all "$TRABAJO/unricopie"
chmod 755 "$TRABAJO/unricopie"

echo "== comprobando el binario"
# Ninguna ruta personal: ni de Linux ni de Windows, ni lo que diga quien llama (el nombre de
# usuario de la máquina). /home/ y /root/ cuentan solo al principio de una ruta: dentro de otra
# son otra cosa (la ruta de D-Bus «/org/a11y/atspi/accessible/root/…» de la accesibilidad).
for patron in '(^|[^A-Za-z0-9_./~-])/(home|root)/' 'C:\Users' '/mnt/c/' "$@"; do
    n="$(strings -a "$TRABAJO/unricopie" | grep -ciE -- "$patron" || true)"
    m="$(strings -a -el "$TRABAJO/unricopie" | grep -ciE -- "$patron" || true)"
    if [ "$n" != 0 ] || [ "$m" != 0 ]; then
        echo "el binario lleva «$patron» ($n veces, $m en UTF-16): no se empaqueta" >&2
        strings -a "$TRABAJO/unricopie" | grep -iE -- "$patron" | cut -c1-300 | head -5 >&2
        exit 1
    fi
done
echo "  rutas: no lleva /home/, /root/, C:\Users ni /mnt/c/$([ $# -gt 0 ] && echo ", ni: $*")"
# La glibc más nueva que pide: la de este contenedor como mucho.
GLIBC="$(objdump -T "$TRABAJO/unricopie" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"
echo "  glibc: pide hasta $GLIBC"
echo "  librerías: $(objdump -p "$TRABAJO/unricopie" | awk '/NEEDED/ {print $2}' | tr '\n' ' ')"

echo "== $NOMBRE-linux-x86_64.tar.gz"
P="$TRABAJO/tar/Unricopie"
mkdir -p "$P/datos"
cp "$TRABAJO/unricopie" "$P/unricopie"
cp /entrada/catalogo.json /entrada/recetas.json "$P/datos/"
cp /entrada/README.txt "$P/README.txt"
cp /src/COPYING "$P/COPYING.txt"
chmod 644 "$P"/datos/*.json "$P"/*.txt
# Sin el dueño de esta máquina, y siempre en el mismo orden.
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$TRABAJO/tar" -czf "/salida/$NOMBRE-linux-x86_64.tar.gz" Unricopie

echo "== $NOMBRE-x86_64.AppImage"
A="$TRABAJO/Unricopie.AppDir"
mkdir -p "$A/usr/bin/datos" "$A/usr/share/applications" "$A/usr/share/icons/hicolor/scalable/apps" "$A/usr/share/icons/hicolor/256x256/apps" "$A/usr/share/doc/unricopie"
cp "$TRABAJO/unricopie" "$A/usr/bin/unricopie"
# La copia del catálogo va junto al ejecutable, que es donde la busca (app/src/datos.rs).
cp /entrada/catalogo.json /entrada/recetas.json "$A/usr/bin/datos/"
cp /entrada/README.txt "$A/usr/share/doc/unricopie/README.txt"
cp /src/COPYING "$A/usr/share/doc/unricopie/COPYING.txt"
cp /src/scripts/linux/unricopie.desktop "$A/usr/share/applications/unricopie.desktop"
cp /src/scripts/linux/unricopie.desktop "$A/unricopie.desktop"
# El pastel: el SVG y un PNG de 256, que es lo que enseñan los gestores de archivos (.DirIcon).
cp /src/app/ui/img/pastel.svg "$A/usr/share/icons/hicolor/scalable/apps/unricopie.svg"
rsvg-convert -w 256 -h 256 /src/app/ui/img/pastel.svg -o "$A/usr/share/icons/hicolor/256x256/apps/unricopie.png"
cp "$A/usr/share/icons/hicolor/256x256/apps/unricopie.png" "$A/unricopie.png"
ln -s unricopie.png "$A/.DirIcon"
ln -s usr/bin/unricopie "$A/AppRun"
find "$A" -type f ! -name unricopie -exec chmod 644 {} +
chmod 755 "$A/usr/bin/unricopie"
find "$A" -type d -exec chmod 755 {} +
desktop-file-validate "$A/unricopie.desktop"
# Con el runtime estático: la AppImage arranca sin libfuse2 (solo pide fusermount o, si no hay
# FUSE, se descomprime con APPIMAGE_EXTRACT_AND_RUN=1). Sin AppStream: no hay metainfo todavía.
ARCH=x86_64 /opt/appimage/appimagetool/AppRun --no-appstream --runtime-file /opt/appimage/runtime-x86_64 "$A" "$TRABAJO/$NOMBRE-x86_64.AppImage" 2>&1 | sed 's/^/  /'
chmod 755 "$TRABAJO/$NOMBRE-x86_64.AppImage"
cp "$TRABAJO/$NOMBRE-x86_64.AppImage" "/salida/$NOMBRE-x86_64.AppImage"

echo "== herramientas"
sed 's/^/  /' /opt/appimage/SHA256SUMS
echo "  $(rustc --version)"
ls -l /salida | sed 's/^/  /'
