#!/usr/bin/env bash
# Builds the release and packs it as wushang.app, then as wushang-<version>-macos-arm64.dmg (the
# app beside a link to Applications, to drag across) and wushang-<version>-macos-arm64.zip, all
# in <target>/bundle. A GitHub release carries both; the in-app updater downloads the disk image.
#
# The app carries its own FFmpeg and libass: every Homebrew dylib the exe links, plus libass
# (loaded at run time, src/ass.rs), and everything those need in turn, is copied into
# Contents/Frameworks and pointed at with @executable_path/../Frameworks, so the app runs on a
# Mac without Homebrew. System libraries stay where they are. The app is signed ad hoc, which
# lets it run on this Mac and, after "Open" in Finder's menu once, on others.
#
# Needs the Command Line Tools (install_name_tool, codesign) and Homebrew's ffmpeg and libass.
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
app="$out/wushang.app"
name="wushang-$version-macos-arm64"

if [ "$build" = 1 ]; then
  # The home folder is cut out of the source paths Rust embeds for panics (the registry crates
  # live under it), so the app doesn't carry it.
  (cd "$root" && cargo build --release \
    --config "target.aarch64-apple-darwin.rustflags=['--remap-path-prefix=$HOME=~']")
fi
exe="${WUSHANG_EXE:-$target_dir/release/wushang}"
[ -x "$exe" ] || { echo "no exe at $exe; build first" >&2; exit 1; }

libass="$(brew --prefix libass 2>/dev/null || echo /opt/homebrew/opt/libass)/lib/libass.9.dylib"
[ -f "$libass" ] || { echo "no libass at $libass: brew install libass" >&2; exit 1; }

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

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/licenses" "$app/Contents/Frameworks"
cp "$exe" "$app/Contents/MacOS/wushang"
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
      # Homebrew's own libraries, for an @rpath with no LC_RPATH that finds them.
      for candidate in /opt/homebrew/lib/"${dep#@rpath/}" /usr/local/lib/"${dep#@rpath/}"; do
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
# Frameworks has been seen to; Homebrew names one library by several paths (opt/ and Cellar/).
bundle_deps() {
  local file="$1" origin="$2" own dep real base
  own="$(otool -D "$file" | tail -n +2)"
  while read -r dep; do
    is_system "$dep" && continue
    case "$dep" in @executable_path/../Frameworks/*) continue ;; esac
    real="$(resolve "$dep" "$file" "$origin")"
    [ -f "$real" ] || { echo "can't find $dep, needed by $origin" >&2; exit 1; }
    base="$(basename "$real")"
    install_name_tool -change "$dep" "@executable_path/../Frameworks/$base" "$file" 2>/dev/null
    if [ ! -f "$frameworks/$base" ]; then
      cp -L "$real" "$frameworks/$base"
      adopt "$frameworks/$base" "$real"
    fi
  done < <(otool -L "$file" | tail -n +2 | awk '{print $1}' | grep -vxF "$own" || true)
}

# Makes the library copied to `$1` from `$2` one of the app's, dependencies and all.
adopt() {
  chmod u+w "$1"
  install_name_tool -id "@executable_path/../Frameworks/$(basename "$1")" "$1" 2>/dev/null
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

# Homebrew builds its libraries for the macOS it runs on, so the app needs that macOS or newer,
# whatever Info.plist says. Said here rather than found out on an older Mac.
wanted="$(plutil -extract LSMinimumSystemVersion raw "$app/Contents/Info.plist")"
needed="$(for f in "$app/Contents/MacOS/wushang" "$frameworks"/*.dylib; do
  otool -l "$f" | awk '/LC_BUILD_VERSION/ {b = 1} b && /minos/ {print $2; exit}'
done | sort -V | tail -n 1)"
if [ "$(printf '%s\n%s\n' "$wanted" "$needed" | sort -V | tail -n 1)" != "$wanted" ]; then
  echo "warning: the libraries were built for macOS $needed, so the app needs it, not $wanted" >&2
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
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

size() { du -sh "$1" | cut -f1; }
echo "$app ($(size "$app"), $(ls "$frameworks" | wc -l | tr -d ' ') libraries)"
echo "$dmg ($(size "$dmg"))"
echo "$zip ($(size "$zip"))"
