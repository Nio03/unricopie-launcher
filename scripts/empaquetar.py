"""Arma el paquete de Unricopie para Windows: `paquetes/Unricopie-<versión>-windows-x64.zip`.

Dentro va la carpeta `Unricopie/` con:
- `unricopie.exe`, compilado en release (con su icono y sus datos de versión);
- `datos/catalogo.json` y `datos/recetas.json`: la copia con la que arranca la primera vez si no
  hay red (después baja las de recompendium.com);
- `README.txt`, en inglés y después en español, y `COPYING.txt`, el texto de la licencia (GPL-3.0).

Uso, desde la carpeta del launcher:
    python scripts/empaquetar.py [--catalogo RUTA|URL] [--recetas RUTA] [--sin-compilar]

Sin `--catalogo`, se baja el de recompendium.com; si todavía no está publicado, se usa el de la
compilación local del sitio (`../unricopie-v3/dist/unricopie/catalogo.json`). Sin `--recetas`, se
exportan de `../unricopie-data` con su `scripts/exportar.py`.
"""
import argparse
import json
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
import zipfile
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent
SITIO = "https://recompendium.com/unricopie"

README = """Unricopie {version}
{raya}

ENGLISH

Unricopie is Recompendium's launcher (https://recompendium.com/unricopie): it installs the
catalog's projects from their official release, checks your copy of the game and gets it ready
to play, mods included.

- Open unricopie.exe. The first time, Windows may warn that it doesn't recognize the app (it
  isn't signed yet): "More info" and "Run anyway".
- Your games and settings are saved in %LOCALAPPDATA%\\Unricopie.
- Portable mode: create an empty file called portable.txt next to unricopie.exe and everything
  will be saved in this folder (games that allow it, too).
- The datos folder holds a copy of the catalog for the first launch without internet; when it
  opens with a connection it downloads the latest one.

It's free software, licensed GPL-3.0-or-later: the text is in COPYING.txt.

ESPAÑOL

Unricopie es el launcher de Recompendium (https://recompendium.com/unricopie): instala los
proyectos del catálogo desde su versión oficial, comprueba tu copia del juego y lo deja listo
para jugar, con sus mods.

- Abre unricopie.exe. La primera vez puede que Windows avise de que no reconoce la aplicación
  (todavía no está firmada): «Más información» y «Ejecutar de todas formas».
- Tus juegos y ajustes se guardan en %LOCALAPPDATA%\\Unricopie.
- Modo portable: crea un archivo vacío llamado portable.txt junto a unricopie.exe y todo se
  guardará en esta carpeta (los juegos que lo permiten, también).
- La carpeta datos trae una copia del catálogo para el primer arranque sin red; al abrirlo con
  conexión baja la última.

Es software libre, con licencia GPL-3.0-or-later: el texto está en COPYING.txt.
"""


def version() -> str:
    with open(RAIZ / "app" / "Cargo.toml", "rb") as f:
        return tomllib.load(f)["package"]["version"]


def catalogo(origen: str | None) -> bytes:
    """El catálogo: el que se pide, el publicado o el de la compilación local del sitio."""
    candidatos = [origen] if origen else [f"{SITIO}/catalogo.json", str(RAIZ.parent / "unricopie-v3" / "dist" / "unricopie" / "catalogo.json")]
    for c in candidatos:
        try:
            if c.startswith("https://"):
                with urllib.request.urlopen(c, timeout=30) as r:
                    datos = r.read()
            else:
                datos = Path(c).read_bytes()
        except Exception as e:  # noqa: BLE001 — se prueba el siguiente
            print(f"  catálogo: {c}: {e}")
            continue
        n = len(json.loads(datos)["entradas"])
        print(f"  catálogo: {c} ({n} fichas)")
        return datos
    sys.exit("no hay catálogo: pasa --catalogo")


def recetas(origen: str | None) -> bytes:
    if origen:
        datos = Path(origen).read_bytes()
    else:
        exportar = RAIZ.parent / "unricopie-data" / "scripts" / "exportar.py"
        if not exportar.is_file():
            sys.exit(f"no está {exportar}: pasa --recetas")
        with tempfile.TemporaryDirectory() as tmp:
            salida = Path(tmp) / "recetas.json"
            # -X utf8: exportar.py escribe «→», que la consola de Windows (cp1252) no acepta.
            subprocess.run([sys.executable, "-X", "utf8", str(exportar), str(salida)], check=True)
            datos = salida.read_bytes()
    n = len(json.loads(datos)["recetas"])
    print(f"  recetas: {n}")
    return datos


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalogo")
    ap.add_argument("--recetas")
    ap.add_argument("--sin-compilar", action="store_true")
    a = ap.parse_args()

    v = version()
    if not a.sin_compilar:
        print("compilando en release…")
        subprocess.run(["cargo", "build", "--release", "-p", "unricopie"], cwd=RAIZ, check=True)
    exe = RAIZ / "target" / "release" / "unricopie.exe"
    if not exe.is_file():
        sys.exit(f"no está {exe}")

    cat = catalogo(a.catalogo)
    rec = recetas(a.recetas)
    destino = RAIZ / "paquetes" / f"Unricopie-{v}-windows-x64.zip"
    destino.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(destino, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(exe, "Unricopie/unricopie.exe")
        z.writestr("Unricopie/datos/catalogo.json", cat)
        z.writestr("Unricopie/datos/recetas.json", rec)
        z.writestr("Unricopie/README.txt", README.format(version=v, raya="=" * len(f"Unricopie {v}")).replace("\n", "\r\n").encode("utf-8-sig"))
        # El texto de la licencia (GPL-3.0), como pide la propia licencia al repartir el programa.
        z.write(RAIZ / "COPYING", "Unricopie/COPYING.txt")
    print(f"listo: {destino} ({destino.stat().st_size / 1048576:.1f} MB)")


if __name__ == "__main__":
    main()
