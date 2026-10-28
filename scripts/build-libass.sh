#!/usr/bin/env bash
# Cross-compiles libass for myvlc on Linux/WSL with MinGW-w64, as one DLL with FreeType, FriBidi
# and HarfBuzz linked in statically; myvlc loads it at run time to draw ASS and SSA subtitles with
# their positions, styles, fonts and karaoke. Fonts come from Windows through DirectWrite, so
# there is no fontconfig.
#
# Needs: gcc-mingw-w64-x86-64-win32 g++-mingw-w64-x86-64-win32 meson ninja-build nasm pkg-config
# Usage: SRC=/path/to/sources PREFIX=/path/to/output scripts/build-libass.sh
# SRC holds the release tarballs unpacked: libass-0.17.3, freetype-2.13.3, fribidi-1.0.16 and
# harfbuzz-10.0.1.
set -euo pipefail

SRC=${SRC:-/mnt/d/tools/src/ass}
PREFIX=${PREFIX:-/mnt/d/tools/libass-0.17.3}
DEPS="$PREFIX-deps"

# The static libraries' .pc files, never the build machine's, answer pkg-config.
export PKG_CONFIG_LIBDIR="$DEPS/lib/pkgconfig"
mkdir -p "$DEPS"
CROSS="$DEPS/cross.meson"
cat > "$CROSS" <<EOF
[binaries]
c = 'x86_64-w64-mingw32-gcc'
cpp = 'x86_64-w64-mingw32-g++'
ar = 'x86_64-w64-mingw32-ar'
strip = 'x86_64-w64-mingw32-strip'
windres = 'x86_64-w64-mingw32-windres'
pkg-config = 'pkg-config'
nasm = 'nasm'

[host_machine]
system = 'windows'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF

# One static library into $DEPS: build directory, source directory, then meson options.
static() {
  local name=$1 src=$2
  shift 2
  rm -rf "$src/build-mingw"
  meson setup "$src/build-mingw" "$src" --cross-file="$CROSS" --prefix="$DEPS" --libdir=lib \
    --buildtype=release --default-library=static "$@"
  ninja -C "$src/build-mingw" install
  echo "built $name"
}

static freetype "$SRC/freetype-2.13.3" \
  -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=disabled \
  -Dtests=disabled
static fribidi "$SRC/fribidi-1.0.16" -Ddocs=false -Dbin=false -Dtests=false
static harfbuzz "$SRC/harfbuzz-10.0.1" \
  -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dchafa=disabled \
  -Dicu=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled

# libass as a DLL with everything inside, the C++ runtime HarfBuzz needs included, so it ships
# alone next to myvlc.exe.
rm -rf "$SRC/libass-0.17.3/build-mingw"
meson setup "$SRC/libass-0.17.3/build-mingw" "$SRC/libass-0.17.3" --cross-file="$CROSS" \
  --prefix="$PREFIX" --libdir=lib --buildtype=release --default-library=shared \
  --prefer-static -Dfontconfig=disabled -Ddirectwrite=enabled -Dcoretext=disabled \
  -Dasm=enabled -Dlibunibreak=disabled \
  -Dc_link_args="-static-libgcc -static-libstdc++ -Wl,-Bstatic -lstdc++ -lwinpthread -Wl,-Bdynamic"
ninja -C "$SRC/libass-0.17.3/build-mingw" install
x86_64-w64-mingw32-strip "$PREFIX"/bin/libass-*.dll
echo "built $(ls "$PREFIX"/bin/libass-*.dll)"
