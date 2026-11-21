# El equipo donde se compila Unricopie para Linux: Ubuntu 22.04, que trae glibc 2.35. Lo compilado
# aquí arranca en esa versión y en las posteriores (Ubuntu 22.04+, Debian 12+, Fedora 36+,
# SteamOS 3.5+, Arch). Lo usa scripts/empaquetar.py --linux; no hace falta construirlo a mano.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
# - build-essential, pkg-config, libfontconfig1-dev: lo que piden Slint y sus dependencias al
#   compilar (X11, Wayland, xkbcommon y OpenGL se cargan al abrir, no al enlazar).
# - file, desktop-file-utils: los usa appimagetool. librsvg2-bin: el icono en PNG.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config libfontconfig1-dev ca-certificates curl \
        file desktop-file-utils librsvg2-bin \
    && rm -rf /var/lib/apt/lists/*

# Rust estable, fuera de /root para que las rutas que deja en el binario se puedan cambiar.
ENV RUSTUP_HOME=/opt/rust/rustup CARGO_HOME=/opt/rust/cargo PATH=/opt/rust/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy --default-toolchain stable

# appimagetool oficial y el runtime estático nuevo (type2-runtime), que no necesita libfuse2 en el
# equipo de la persona. appimagetool es una AppImage: se deja descomprimida, porque en el
# contenedor no hay FUSE.
RUN mkdir -p /opt/appimage && cd /opt/appimage \
    && curl -fsSL -o appimagetool.AppImage https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage \
    && curl -fsSL -o runtime-x86_64 https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 \
    && sha256sum appimagetool.AppImage runtime-x86_64 > SHA256SUMS \
    && chmod +x appimagetool.AppImage \
    && ./appimagetool.AppImage --appimage-extract >/dev/null \
    && mv squashfs-root appimagetool && rm appimagetool.AppImage
