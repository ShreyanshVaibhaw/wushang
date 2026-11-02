#!/usr/bin/env bash
# Builds libass for wushang on macOS (Apple Silicon), for macOS 13 and later, as one dylib with
# FreeType, FriBidi and HarfBuzz linked in statically, the way scripts/build-libass.sh does for
# Windows; wushang loads it at run time (src/ass.rs) to draw ASS and SSA subtitles with their
# positions, styles, fonts and karaoke. Fonts come from macOS through Core Text, so there is no
# fontconfig. The dylib is named @rpath/libass.9.dylib and needs only system libraries.
#
# The tarballs are downloaded into SRC when they aren't there and checked against the SHA-256
# sums below (libass's and FreeType's tarballs are GPG-signed by their maintainers; the sums are
# of the ones whose signatures checked out). Each run unpacks them afresh.
#
# Needs the Command Line Tools and Homebrew's meson, ninja and pkgconf.
# Usage: SRC=/path/to/sources PREFIX=/path/to/output scripts/build-libass-macos.sh
# SRC defaults to ../tools/src/macos and PREFIX to ../tools/libass-0.17.5, beside the
# repository.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tools="$(cd "$root/.." && pwd)/tools"
SRC=${SRC:-$tools/src/macos}
PREFIX=${PREFIX:-$tools/libass-0.17.5}
DEPS="$PREFIX-deps"
export MACOSX_DEPLOYMENT_TARGET=13.0

# Fetches a tarball into $SRC unless it is there, checks it, and unpacks it into $SRC/<name>.
fetch() {
  local name=$1 url=$2 sum=$3 file
  file="$SRC/$(basename "$url")"
  mkdir -p "$SRC"
  [ -f "$file" ] || curl -fL --retry 3 -o "$file" "$url"
  echo "$sum  $file" | shasum -a 256 -c - >/dev/null ||
    { echo "$file doesn't match its SHA-256" >&2; exit 1; }
  rm -rf "${SRC:?}/$name"
  tar xf "$file" -C "$SRC"
}

fetch freetype-2.15.0 https://downloads.sourceforge.net/project/freetype/freetype2/2.15.0/freetype-2.15.0.tar.xz \
  93cd47eb719c325107f2d3cf087d06299e944c3b48a40b55537302304dea1dd5
fetch fribidi-1.0.18 https://github.com/fribidi/fribidi/releases/download/v1.0.18/fribidi-1.0.18.tar.xz \
  1de5e8073d035388b71d0b2d452b1da1f8caeea8a7702997b401fe1b6250727f
fetch harfbuzz-15.0.0 https://github.com/harfbuzz/harfbuzz/releases/download/15.0.0/harfbuzz-15.0.0.tar.xz \
  968536e9abd782f07f7954044c0c4020199fcdd39b41d221ec42d2a3be4a44df
fetch libass-0.17.5 https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz \
  2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc

rm -rf "$PREFIX" "$DEPS"

# The static libraries' .pc files, never Homebrew's, answer pkg-config.
export PKG_CONFIG_LIBDIR="$DEPS/lib/pkgconfig"

# One static library into $DEPS: its name, then meson options.
static() {
  local src="$SRC/$1"
  shift
  meson setup "$src/build" "$src" --prefix="$DEPS" --libdir=lib \
    --buildtype=release --default-library=static "$@"
  ninja -C "$src/build" install
}

# No HVF: Apple's libhvf, which FreeType would link for Apple's hierarchical variable fonts, is
# only in macOS 26 and later.
static freetype-2.15.0 \
  -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=disabled \
  -Dhvf=disabled -Dtests=disabled
static fribidi-1.0.18 -Ddocs=false -Dbin=false -Dtests=false
# Shaping only: no font drawing, subsetting or other platform backends, which libass doesn't use.
static harfbuzz-15.0.0 \
  -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dchafa=disabled \
  -Dpng=disabled -Dzlib=disabled -Dicu=disabled -Dgraphite=disabled -Dgraphite2=disabled \
  -Dfontations=disabled -Dharfrust=disabled -Dkbts=disabled -Dwasm=disabled \
  -Dcoretext=disabled -Dgdi=disabled -Ddirectwrite=disabled \
  -Draster=disabled -Dvector=disabled -Dgpu=disabled -Dgpu_demo=disabled -Dsubset=disabled \
  -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dbenchmark=disabled \
  -Dintrospection=disabled

# libass is built static too, then linked into a dylib by hand. meson's own libass dylib exports
# every symbol on macOS, FreeType's and HarfBuzz's too; this one exports libass.sym, the list
# libass's autotools build exports, and the linker drops the rest of what goes unused. iconv is
# macOS's; libass uses it for fonts whose only character maps are legacy CJK ones.
static libass-0.17.5 \
  -Dfontconfig=disabled -Dcoretext=enabled -Ddirectwrite=disabled -Drequire-system-font-provider=true \
  -Dasm=enabled -Dlibunibreak=disabled -Dtest=disabled -Dcompare=disabled -Dprofile=disabled \
  -Dfuzz=disabled -Dcheckasm=disabled
grep -q '^#define CONFIG_ICONV 1' "$SRC/libass-0.17.5/build/config.h" ||
  { echo "libass has no iconv" >&2; exit 1; }

mkdir -p "$PREFIX/lib" "$PREFIX/include"
cp -R "$DEPS/include/ass" "$PREFIX/include/"
sed 's/^/_/' "$SRC/libass-0.17.5/libass/libass.sym" > "$DEPS/libass.exports"
lib="$PREFIX/lib/libass.9.dylib"
# The versions are the ones libass's own build gives 0.17.5 (libtool's 13:2:4); the header pad
# leaves room for scripts/bundle-macos.sh to rename it.
# shellcheck disable=SC2046 # pkg-config's flags are words
clang -dynamiclib -o "$lib" -install_name @rpath/libass.9.dylib \
  -compatibility_version 14 -current_version 14.2 \
  -Wl,-force_load,"$DEPS/lib/libass.a" $(pkg-config --static --libs libass) \
  -Wl,-exported_symbols_list,"$DEPS/libass.exports" -Wl,-dead_strip -Wl,-x \
  -Wl,-headerpad_max_install_names
ln -sf libass.9.dylib "$PREFIX/lib/libass.dylib"

minos="$(otool -l "$lib" | awk '/LC_BUILD_VERSION/ {b = 1} b && /minos/ {print $2; exit}')"
[ "$minos" = 13.0 ] || { echo "$lib is for macOS $minos" >&2; exit 1; }
if otool -L "$lib" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/|@rpath/libass)'; then
  echo "$lib needs a library macOS doesn't have" >&2; exit 1
fi
echo "built $lib: $(du -k "$lib" | cut -f1) KB, macOS $minos"
