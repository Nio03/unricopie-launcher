"""Arma los paquetes de Unricopie: el de Windows (`paquetes/Unricopie-<versión>-windows-x64.zip`)
o, con `--linux`, los dos de Linux (`Unricopie-<versión>-linux-x86_64.tar.gz` y
`Unricopie-<versión>-x86_64.AppImage`).

Dentro va la carpeta `Unricopie/` con:
- `unricopie.exe` (`unricopie` en Linux), compilado en release (con su icono y sus datos de versión);
- `datos/catalogo.json` y `datos/recetas.json`: la copia con la que arranca la primera vez si no
  hay red (después baja las de recompendium.com);
- `README.txt`, en inglés y después en español, y `COPYING.txt`, el texto de la licencia (GPL-3.0).

Uso, desde la carpeta del launcher:
    python scripts/empaquetar.py [--linux] [--catalogo RUTA|URL] [--recetas RUTA] [--sin-compilar]

Sin `--catalogo`, se baja el de recompendium.com; si todavía no está publicado, se usa el de la
compilación local del sitio (`../unricopie-v3/dist/unricopie/catalogo.json`). Sin `--recetas`, se
exportan de `../unricopie-data` con su `scripts/exportar.py`.

El exe se compila sin las rutas de la máquina (las de la carpeta personal, la de Cargo y la del
proyecto, que Rust deja en los mensajes de error) y, si aun así queda alguna, no se empaqueta.

Los de Linux se compilan y se arman dentro de un contenedor de Docker con Ubuntu 22.04
(`scripts/linux/Dockerfile` y `scripts/linux/empaquetar.sh`), así que hace falta Docker y nada más:
vale desde Windows. Lo compilado ahí pide glibc 2.35 como mucho, y por tanto arranca en Ubuntu
22.04, Debian 12, Fedora 36, SteamOS 3.5 y lo que vino después. La AppImage lleva el runtime
estático de AppImage, que no necesita libfuse2. El binario va sin símbolos y se comprueba igual
que el exe. La caché de la compilación queda en el volumen de Docker `unricopie-linux-target`.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
import zipfile
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent
SITIO = "https://recompendium.com/unricopie"
# Carpeta propia para la compilación del paquete: cambiar RUSTFLAGS invalida la caché, y así no
# se pierde la de `cargo build --release` normal.
OBJETIVO = RAIZ / "target" / "paquete"

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

# Lo mismo para Linux. Va con saltos de línea de Unix y sin BOM.
README_LINUX = """Unricopie {version}
{raya}

ENGLISH

Unricopie is Recompendium's launcher (https://recompendium.com/unricopie): it installs the
catalog's projects from their official release, checks your copy of the game and gets it ready
to play, mods included.

- Run ./unricopie (or the .AppImage: make it executable first, chmod +x). It needs glibc 2.35
  or newer (Ubuntu 22.04, Debian 12, Fedora 36, SteamOS 3.5) and a desktop with OpenGL.
- Your games and settings are saved in ~/.local/share/unricopie.
- Portable mode: create an empty file called portable.txt next to unricopie (or next to the
  .AppImage) and everything will be saved in that folder (games that allow it, too).
- The datos folder holds a copy of the catalog for the first launch without internet; when it
  opens with a connection it downloads the latest one.
- Games are the projects' own Linux builds. If one needs a system library you don't have
  (SDL2, for instance), Unricopie tells you which one and how to install it.

It's free software, licensed GPL-3.0-or-later: the text is in COPYING.txt.

ESPAÑOL

Unricopie es el launcher de Recompendium (https://recompendium.com/unricopie): instala los
proyectos del catálogo desde su versión oficial, comprueba tu copia del juego y lo deja listo
para jugar, con sus mods.

- Abre ./unricopie (o la .AppImage: antes hazla ejecutable, chmod +x). Pide glibc 2.35 o
  posterior (Ubuntu 22.04, Debian 12, Fedora 36, SteamOS 3.5) y un escritorio con OpenGL.
- Tus juegos y ajustes se guardan en ~/.local/share/unricopie.
- Modo portable: crea un archivo vacío llamado portable.txt junto a unricopie (o junto a la
  .AppImage) y todo se guardará en esa carpeta (los juegos que lo permiten, también).
- La carpeta datos trae una copia del catálogo para el primer arranque sin red; al abrirlo con
  conexión baja la última.
- Los juegos son las versiones de Linux de cada proyecto. Si a alguno le falta una librería del
  sistema (SDL2, por ejemplo), Unricopie te dice cuál y cómo instalarla.

Es software libre, con licencia GPL-3.0-or-later: el texto está en COPYING.txt.
"""

IMAGEN_LINUX = "unricopie-linux-build"


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


def rutas_privadas() -> dict[str, str]:
    """Rutas de esta máquina que no deben quedar en el exe, con el nombre que las sustituye.
    Si varias encajan, rustc aplica la última: van de la más general a la más concreta."""
    casa = Path.home()
    cargo = Path(os.environ.get("CARGO_HOME") or casa / ".cargo")
    rustup = Path(os.environ.get("RUSTUP_HOME") or casa / ".rustup")
    return {str(casa): "~", str(rustup): "rustup", str(cargo): "cargo", str(RAIZ): "unricopie-launcher"}


def compilar() -> Path:
    print("compilando en release, sin las rutas de la máquina…")
    flags = " ".join(f"--remap-path-prefix={de}={a}" for de, a in rutas_privadas().items())
    entorno = {**os.environ, "RUSTFLAGS": flags}
    subprocess.run(["cargo", "build", "--release", "-p", "unricopie", "--target-dir", str(OBJETIVO)], cwd=RAIZ, check=True, env=entorno)
    return OBJETIVO / "release" / "unricopie.exe"


def comprobar_rutas(exe: Path) -> None:
    """Falla si el exe todavía lleva la carpeta personal o el nombre de usuario de esta máquina."""
    datos = exe.read_bytes()
    buscar = {str(Path.home()), Path.home().name}
    hallazgos = {}
    for texto in buscar:
        for codif in ("utf-8", "utf-16-le"):
            n = datos.lower().count(texto.lower().encode(codif))
            if n:
                hallazgos[f"{texto} ({codif})"] = n
    if hallazgos:
        sys.exit(f"el exe lleva rutas de esta máquina, no se empaqueta: {hallazgos}")
    print("  rutas: el exe no lleva la carpeta personal ni el nombre de usuario")


def linux(v: str, cat: bytes, rec: bytes, sin_compilar: bool) -> None:
    """Los dos paquetes de Linux, compilados y armados en el contenedor de Ubuntu 22.04."""
    if sin_compilar:
        sys.exit("--sin-compilar no vale con --linux: el contenedor compila con su propia caché")
    # Lo que no depende del sistema se prepara aquí y se le pasa al contenedor.
    entrada = RAIZ / "target" / "paquete-linux" / "entrada"
    entrada.mkdir(parents=True, exist_ok=True)
    (entrada / "catalogo.json").write_bytes(cat)
    (entrada / "recetas.json").write_bytes(rec)
    (entrada / "README.txt").write_bytes(README_LINUX.format(version=v, raya="=" * len(f"Unricopie {v}")).encode("utf-8"))
    salida = RAIZ / "paquetes"
    salida.mkdir(exist_ok=True)

    print("preparando el contenedor de compilación (ubuntu:22.04)…")
    subprocess.run(["docker", "build", "-q", "-t", IMAGEN_LINUX, str(RAIZ / "scripts" / "linux")], check=True)
    subprocess.run(
        [
            "docker", "run", "--rm",
            "-v", f"{RAIZ}:/src:ro",
            "-v", f"{entrada}:/entrada:ro",
            "-v", f"{salida}:/salida",
            "-v", "unricopie-linux-target:/target",
            "-v", "unricopie-linux-cargo:/opt/rust/cargo/registry",
            IMAGEN_LINUX, "bash", "/src/scripts/linux/empaquetar.sh", v,
            # Tampoco puede quedar el nombre de usuario de esta máquina.
            Path.home().name,
        ],
        check=True,
    )
    for nombre in (f"Unricopie-{v}-linux-x86_64.tar.gz", f"Unricopie-{v}-x86_64.AppImage"):
        print(f"listo: {salida / nombre} ({(salida / nombre).stat().st_size / 1048576:.1f} MB)")


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalogo")
    ap.add_argument("--recetas")
    ap.add_argument("--sin-compilar", action="store_true")
    ap.add_argument("--linux", action="store_true", help="los paquetes de Linux (tar.gz y AppImage), con Docker")
    a = ap.parse_args()

    v = version()
    if a.linux:
        return linux(v, catalogo(a.catalogo), recetas(a.recetas), a.sin_compilar)
    exe = compilar() if not a.sin_compilar else OBJETIVO / "release" / "unricopie.exe"
    if not exe.is_file():
        sys.exit(f"no está {exe}")
    comprobar_rutas(exe)

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
