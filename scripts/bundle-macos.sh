#!/usr/bin/env bash
# Builds the release and packs it as wushang.app, then as wushang-<version>-macos-arm64.dmg (the
# app beside a link to Applications, to drag across) and wushang-<version>-macos-arm64.zip, all
# in <target>/bundle. A GitHub release carries both; the in-app updater downloads the disk image.
#
# The app carries its own FFmpeg and libass, the slim LGPL builds of
# scripts/build-ffmpeg-macos.sh and scripts/build-libass-macos.sh: every dylib the exe links,
# plus libass (loaded at run time, src/ass.rs), and everything those need in turn, is copied
# into Contents/Frameworks and pointed at with @executable_path/../Frameworks, so the app runs
# on a Mac without them. System libraries stay where they are. The app is signed ad hoc, which
# lets it run on this Mac and, after "Open" in Finder's menu once, on others.
#
# Needs the Command Line Tools (install_name_tool, codesign) and the two builds, in FFMPEG_DIR
# and LIBASS_DIR (by default ../tools/ffmpeg-9.0.2-wushang and ../tools/libass-0.17.5 beside
# the repository, where the build scripts put them).
#
#   scripts/bundle-macos.sh              build, bundle, sign, pack
#   scripts/bundle-macos.sh --no-build   bundle the release exe already built
#   WUSHANG_EXE=/path/to/exe scripts/bundle-macos.sh --no-build
#                                        bundle another exe (to try the dylib bundling alone)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build=1
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    *) echo "unknown option $arg" >&2; exit 2 ;;
  esac
done

meta="$(cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml")"
target_dir="$(printf '%s' "$meta" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
[ -n "$target_dir" ] && [ -n "$version" ] || { echo "can't read cargo metadata" >&2; exit 1; }
out="$target_dir/bundle"
# The app is kept in a .noindex folder: Spotlight skips it, and wushang doesn't register a copy
# there with Launch Services, so this build never shows beside the installed app in Spotlight
# or "Open With". The disk image and zip carry it out.
app="$out/app.noindex/wushang.app"
name="wushang-$version-macos-arm64"

tools="$(cd "$root/.." && pwd)/tools"
ffmpeg_dir="${FFMPEG_DIR:-$tools/ffmpeg-9.0.2-wushang}"
ffmpeg_lib="$ffmpeg_dir/lib"
libass="${LIBASS_DIR:-$tools/libass-0.17.5}/lib/libass.9.dylib"
[ -f "$ffmpeg_lib/libavcodec.dylib" ] ||
  { echo "no FFmpeg in $ffmpeg_lib: run scripts/build-ffmpeg-macos.sh" >&2; exit 1; }
[ -f "$libass" ] || { echo "no libass at $libass: run scripts/build-libass-macos.sh" >&2; exit 1; }

if [ "$build" = 1 ]; then
  # Linked against that FFmpeg (ffmpeg-sys-next takes FFMPEG_DIR over pkg-config, which would
  # find Homebrew's) and built for the macOS Info.plist names. The home folder is cut out of the
  # source paths Rust embeds for panics (the registry crates live under it), so the app doesn't
  # carry it.
  (cd "$root" && FFMPEG_DIR="$ffmpeg_dir" MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release \
    --config "target.aarch64-apple-darwin.rustflags=['--remap-path-prefix=$HOME=~']")
fi
exe="${WUSHANG_EXE:-$target_dir/release/wushang}"
[ -x "$exe" ] || { echo "no exe at $exe; build first" >&2; exit 1; }

# The icon: assets/wushang.svg drawn on Apple's icon grid (the rounded square 824 of 1024 pixels,
# centred) at every size an .icns holds. Made again only when the SVG changes.
icns="$root/assets/macos/wushang.icns"
if [ ! -f "$icns" ] || [ "$root/assets/wushang.svg" -nt "$icns" ]; then
  work="$(mktemp -d)"
  mkdir "$work/wushang.iconset"
  for size in 16 32 128 256 512; do
    for scale in 1 2; do
      px=$((size * scale))
      suffix=$([ "$scale" = 2 ] && echo "@2x" || true)
      # The SVG's square spans 0.019 to 0.981 of its box; scaled by 0.8353 and moved in by
      # 0.0817 it spans Apple's 100 to 924 of 1024.
      perl -0pe 's/<svg[^>]*>/<svg width="'$px'" height="'$px'" viewBox="0 0 1 1" xmlns="http:\/\/www.w3.org\/2000\/svg"><g transform="translate(0.08167 0.08167) scale(0.8353)">/; s/<\/svg>\s*$/<\/g><\/svg>\n/' \
        "$root/assets/wushang.svg" > "$work/icon.svg"
      sips -s format png "$work/icon.svg" --out "$work/wushang.iconset/icon_${size}x${size}${suffix}.png" >/dev/null
    done
  done
  iconutil -c icns -o "$icns" "$work/wushang.iconset"
  rm -rf "$work"
fi

rm -rf "$app" "$out/wushang.app" # the second is where older runs of this script left it
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/licenses" "$app/Contents/Frameworks"
cp "$exe" "$app/Contents/MacOS/wushang"

# install_name_tool, which warns on every change that it breaks the file's signature (the app is
# signed again below), so its output shows only when it fails.
name_tool() {
  local out
  out="$(install_name_tool "$@" 2>&1)" || { echo "install_name_tool $*: $out" >&2; return 1; }
}

# The app looks for libraries only in itself, so any rpath that isn't relative goes: one to
# FFmpeg's folder, say, that a build for cargo run and cargo test was linked with.
while read -r rpath; do
  case "$rpath" in @*) ;; *) name_tool -delete_rpath "$rpath" "$app/Contents/MacOS/wushang" ;; esac
done < <(otool -l "$exe" | awk '/cmd LC_RPATH/ {getline; getline; print $2}')

sed "s/@VERSION@/$version/g" "$root/assets/macos/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "$icns" "$app/Contents/Resources/wushang.icns"
cp "$root/LICENSE" "$root/THIRD_PARTY_NOTICES.md" "$app/Contents/Resources/"
cp "$root/assets/fonts/OFL.txt" "$app/Contents/Resources/Inter-OFL.txt"
cp "$root/licenses/"* "$app/Contents/Resources/licenses/"

frameworks="$app/Contents/Frameworks"

# Where a dependency `dep` of the Mach-O file `from` (which sits at `origin`, its path before it
# was copied) really is: absolute paths as they are, @loader_path from `origin`'s folder, and
# @rpath through `from`'s LC_RPATH entries.
resolve() {
  local dep="$1" from="$2" origin="$3" dir rpath candidate
  dir="$(dirname "$origin")"
  case "$dep" in
    @loader_path/*) echo "$dir/${dep#@loader_path/}" ;;
    @executable_path/*) echo "$(dirname "$exe")/${dep#@executable_path/}" ;;
    @rpath/*)
      while read -r rpath; do
        rpath="${rpath/@loader_path/$dir}"
        rpath="${rpath/@executable_path/$(dirname "$exe")}"
        candidate="$rpath/${dep#@rpath/}"
        if [ -f "$candidate" ]; then echo "$candidate"; return; fi
      done < <(otool -l "$from" | awk '/cmd LC_RPATH/ {getline; getline; print $2}')
      # The FFmpeg and libass builds name each other by @rpath and carry no LC_RPATH.
      for candidate in "$ffmpeg_lib/${dep#@rpath/}" "$(dirname "$libass")/${dep#@rpath/}"; do
        if [ -f "$candidate" ]; then echo "$candidate"; return; fi
      done
      echo "$dep" ;;
    *) echo "$dep" ;;
  esac
}

# Whether a dependency comes with macOS, which every Mac has.
is_system() {
  case "$1" in
    /System/*|/usr/lib/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Copies every non-system dependency of the file at `$1` (copied from `$2`) into Frameworks,
# points the file at the copies, and does the same for each copy. A library already in
# Frameworks has been seen to; one library can be named by several paths (symlinks).
bundle_deps() {
  local file="$1" origin="$2" own dep real base
  own="$(otool -D "$file" | tail -n +2)"
  while read -r dep; do
    is_system "$dep" && continue
    case "$dep" in @executable_path/../Frameworks/*) continue ;; esac
    real="$(resolve "$dep" "$file" "$origin")"
    [ -f "$real" ] || { echo "can't find $dep, needed by $origin" >&2; exit 1; }
    # Only the two LGPL builds go in; an exe linked against Homebrew's GPL FFmpeg stops here.
    case "$real" in
      "$ffmpeg_lib"/*|"$(dirname "$libass")"/*) ;;
      *) echo "$origin needs $real, which isn't from FFMPEG_DIR or LIBASS_DIR" >&2; exit 1 ;;
    esac
    base="$(basename "$real")"
    name_tool -change "$dep" "@executable_path/../Frameworks/$base" "$file"
    if [ ! -f "$frameworks/$base" ]; then
      cp -L "$real" "$frameworks/$base"
      adopt "$frameworks/$base" "$real"
    fi
  done < <(otool -L "$file" | tail -n +2 | awk '{print $1}' | grep -vxF "$own" || true)
}

# Makes the library copied to `$1` from `$2` one of the app's, dependencies and all.
adopt() {
  chmod u+w "$1"
  name_tool -id "@executable_path/../Frameworks/$(basename "$1")" "$1"
  bundle_deps "$1" "$2"
}

cp -L "$libass" "$frameworks/"
adopt "$frameworks/$(basename "$libass")" "$libass"
bundle_deps "$app/Contents/MacOS/wushang" "$exe"

# Nothing may still point outside the app or the system.
for f in "$app/Contents/MacOS/wushang" "$frameworks"/*.dylib; do
  while read -r dep; do
    is_system "$dep" && continue
    case "$dep" in
      @executable_path/../Frameworks/*)
        [ -f "$frameworks/${dep#@executable_path/../Frameworks/}" ] ||
          { echo "$f needs $dep, which isn't in the app" >&2; exit 1; } ;;
      *) echo "$f still needs $dep" >&2; exit 1 ;;
    esac
  done < <(otool -L "$f" | tail -n +2 | awk '{print $1}')
done

# The app needs the newest macOS any of its parts was built for, whatever Info.plist says; a
# library from Homebrew, built for the Mac's own macOS, would raise it. Said here rather than
# found out on an older Mac.
wanted="$(plutil -extract LSMinimumSystemVersion raw "$app/Contents/Info.plist")"
needed="$(for f in "$app/Contents/MacOS/wushang" "$frameworks"/*.dylib; do
  otool -l "$f" | awk '/LC_BUILD_VERSION/ {b = 1} b && /minos/ {print $2; exit}'
done | sort -V | tail -n 1)"
if [ "$(printf '%s\n%s\n' "$wanted" "$needed" | sort -V | tail -n 1)" != "$wanted" ]; then
  echo "the libraries were built for macOS $needed, so the app needs it, not $wanted" >&2
  exit 1
fi

# A part can say macOS 13 and still not load there: dyld stops at launch on a strong import the
# running system lacks. Every strong import of the exe and its libraries is compiled against the
# minimum, and clang names any that are newer. It reads each framework's every header, since an
# umbrella header can leave one out, as CoreAudio.h does AudioHardwareTapping.h.
imports="$(mktemp -d)/imports.m"
sdk="$(xcrun --show-sdk-path)"
{
  for f in "$app/Contents/MacOS/wushang" "$frameworks"/*.dylib; do otool -L "$f"; done |
    sed -n 's|.*/System/Library/Frameworks/\([^./]*\)\.framework/.*|\1|p' | sort -u |
    while read -r framework; do
      for header in "$sdk/System/Library/Frameworks/$framework.framework/Headers"/*.h; do
        [ -f "$header" ] && printf '#import <%s/%s>\n' "$framework" "$(basename "$header")"
      done
    done
  echo 'void use(void *);'
  echo 'void check(void) {'
  for f in "$app/Contents/MacOS/wushang" "$frameworks"/*.dylib; do dyld_info -imports "$f"; done |
    awk '/ \(from / && !/weak-import/ {print substr($2, 2)}' | grep -E '^[A-Za-z_][A-Za-z0-9_]*$' |
    sort -u | sed 's/.*/use((void *)\&&);/'
  echo '}'
} > "$imports"
scan="$(clang -fsyntax-only -ferror-limit=0 -Wunguarded-availability-new \
  -target "arm64-apple-macos$wanted" "$imports" 2>&1 || true)"
rm -rf "$(dirname "$imports")"
# Names no header declares are expected; a scan that stopped early proves nothing.
if grep -q 'fatal error:' <<<"$scan"; then
  echo "can't check the app's imports:" >&2
  grep 'fatal error:' <<<"$scan" >&2
  exit 1
fi
too_new="$(grep 'is only available on' <<<"$scan" || true)"
if [ -n "$too_new" ]; then
  echo "the app wouldn't load on macOS $wanted:" >&2
  echo "$too_new" >&2
  exit 1
fi

# Signed inside out: each library, then the app, whose seal covers them. install_name_tool
# broke the signatures the libraries came with.
for f in "$frameworks"/*.dylib; do
  codesign --force --sign - --timestamp=none "$f" 2>/dev/null
done
codesign --force --sign - --timestamp=none "$app" 2>/dev/null
codesign --verify --deep --strict "$app"

dmg="$out/$name.dmg"
zip="$out/$name.zip"
stage="$(mktemp -d)"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname wushang -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"
rm -rf "$stage"
# Launch Services may have met the app while it was built (codesign, Finder); forget this copy.
"/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister" -u "$app" 2>/dev/null || true
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

size() { du -sh "$1" | cut -f1; }
echo "$app ($(size "$app"), $(ls "$frameworks" | wc -l | tr -d ' ') libraries)"
echo "$dmg ($(size "$dmg"))"
echo "$zip ($(size "$zip"))"
