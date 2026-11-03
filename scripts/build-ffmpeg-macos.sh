#!/usr/bin/env bash
# Builds a slim LGPL FFmpeg for wushang on macOS (Apple Silicon), for macOS 13 and later, the
# way scripts/build-ffmpeg.sh does for Windows: the same demuxers, parsers, decoders, muxers and
# protocols, with macOS's own pieces standing in for Windows'. VideoToolbox decodes on the GPU
# and encodes H.264 and HEVC, AudioToolbox encodes AAC, AVFoundation captures cameras,
# microphones and screens, and TLS for https comes from macOS's Secure Transport, so there is
# no OpenSSL. dav1d decodes AV1 in software where the GPU can't, LAME encodes MP3 (both linked
# in statically) and zlib, which macOS has, reads Matroska tracks stored compressed. ProRes
# decodes too, on the GPU where it can. No libbluray: wushang reads Blu-ray playlists itself
# (src/disc.rs) and plays them through `concat`, so FFmpeg's bluray protocol would go unused.
# Without --enable-gpl, --enable-version3 or --enable-nonfree the build is LGPL v2.1 or later.
#
# The tarballs are downloaded into SRC when they aren't there and checked against the SHA-256
# sums below (FFmpeg's and dav1d's tarballs are GPG-signed by their release keys; the sums are
# of the ones whose signatures checked out). Each run unpacks them afresh. The dylibs are named
# @rpath/lib*.dylib, so an exe finds them through its LC_RPATH (build.rs adds
# @executable_path/../Frameworks for the app bundle), with room in their load commands for
# scripts/bundle-macos.sh to rename them.
#
# Needs the Command Line Tools and Homebrew's meson, ninja and pkgconf.
# Usage: SRC=/path/to/sources PREFIX=/path/to/output scripts/build-ffmpeg-macos.sh
# SRC defaults to ../tools/src/macos and PREFIX to ../tools/ffmpeg-9.0.2-wushang, beside the
# repository.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tools="$(cd "$root/.." && pwd)/tools"
SRC=${SRC:-$tools/src/macos}
PREFIX=${PREFIX:-$tools/ffmpeg-9.0.2-wushang}
# Static libraries linked into the FFmpeg dylibs: none of them ships as a dylib of its own.
DEPS="$PREFIX-deps"
export MACOSX_DEPLOYMENT_TARGET=13.0
jobs="$(sysctl -n hw.ncpu)"

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

fetch ffmpeg-9.0.2 https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz \
  8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
fetch dav1d-1.5.4 https://downloads.videolan.org/pub/videolan/dav1d/1.5.4/dav1d-1.5.4.tar.xz \
  686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5
fetch lame-4.0 https://downloads.sourceforge.net/project/lame/lame/4.0/lame-4.0.tar.gz \
  3df5124d5ad3a98312ffd7ba6a9b36230e4f8a3e66d3ce0f425e336c32d216eb

rm -rf "$PREFIX" "$DEPS"

# dav1d (BSD-2) decodes AV1 on GPUs that can't (Apple's decode it from M3 on).
meson setup "$SRC/dav1d-1.5.4/build" "$SRC/dav1d-1.5.4" --prefix="$DEPS" --libdir=lib \
  --buildtype=release --default-library=static \
  -Denable_tools=false -Denable_tests=false
ninja -C "$SRC/dav1d-1.5.4/build" install

# LAME (LGPL v2) encodes MP3s for Convert (src/convert.rs); without it they come out as M4A.
# Only the encoding library: no lame command, no mpg123 decoder.
(cd "$SRC/lame-4.0" && ./configure --prefix="$DEPS" --enable-static --disable-shared \
  --disable-frontend --disable-decoder --disable-dependency-tracking &&
  make -j"$jobs" && make install)

# Only the static libraries' .pc files, never Homebrew's, may answer FFmpeg's pkg-config checks.
export PKG_CONFIG_LIBDIR="$DEPS/lib/pkgconfig"

join() { local IFS=,; echo "$*"; }

demuxers=(mov matroska avi mpegts mpegps flv ogg asf ivf obu
          wav w64 aiff caf mp3 aac ac3 eac3 dts truehd flac
          h264 hevc m4v mpegvideo
          srt ass webvtt microdvd subviewer sami vobsub sup
          hls rtsp sdp
          ape)
protocols=(file data http https tls tcp udp rtp crypto concat)
# ipod is the M4A flavour of MP4, which Convert writes when it has no MP3 encoder.
muxers=(matroska webm mov mp4 ipod mpegts adts mp3 wav flac)
# Convert tries macOS's encoders first and falls back on FFmpeg's own AAC one.
encoders=(h264_videotoolbox hevc_videotoolbox aac_at aac libmp3lame)
indevs=(avfoundation)
parsers=(h264 hevc av1 vp8 vp9 mpeg4video mpegvideo vc1 mjpeg
         aac aac_latm ac3 mpegaudio opus vorbis flac dca mlp
         dvdsub dvbsub)
# The Windows list, plus ProRes, the PCM layouts a Core Audio device can deliver, and the sound
# of two kinds of file wushang lists that the Windows build can't play: Monkey's Audio (.ape,
# with its demuxer above) and AMR, which most .3gp videos carry. PNG is for cover art, which
# music files embed as PNG about as often as JPEG.
decoders=(h264 hevc av1 libdav1d vp8 vp9 mpeg4 mpeg2video mpeg1video vc1 wmv3 msmpeg4v3 mjpeg theora rawvideo
          prores ape amrnb amrwb png
          aac aac_latm mp3float mp2float ac3 eac3 opus vorbis flac alac dca truehd wmav2
          pcm_s16le pcm_s16be pcm_s24le pcm_s24be pcm_s32le pcm_s32be pcm_f32le pcm_f32be
          pcm_u8 pcm_alaw pcm_mulaw pcm_bluray pcm_dvd
          subrip srt ass ssa webvtt movtext text microdvd subviewer sami dvdsub dvbsub pgssub)
# The codecs src/player.rs hands to VideoToolbox.
hwaccels=()
for codec in h264 hevc av1 vp9 mpeg2 mpeg1 prores; do hwaccels+=("${codec}_videotoolbox"); done

cd "$SRC/ffmpeg-9.0.2"
./configure \
  --prefix="$PREFIX" --install-name-dir=@rpath \
  --enable-shared --disable-static \
  --disable-programs --disable-doc --disable-debug \
  --disable-avfilter \
  --disable-autodetect --enable-pthreads \
  --enable-videotoolbox --enable-audiotoolbox --enable-avfoundation \
  --enable-network --enable-securetransport \
  --enable-libdav1d --enable-libmp3lame --enable-zlib \
  --pkg-config=pkg-config --pkg-config-flags=--static \
  --extra-cflags="-I$DEPS/include" --extra-ldflags="-L$DEPS/lib -Wl,-headerpad_max_install_names" \
  --disable-everything \
  --enable-protocol="$(join "${protocols[@]}")" \
  --enable-demuxer="$(join "${demuxers[@]}")" \
  --enable-parser="$(join "${parsers[@]}")" \
  --enable-decoder="$(join "${decoders[@]}")" \
  --enable-muxer="$(join "${muxers[@]}")" \
  --enable-encoder="$(join "${encoders[@]}")" \
  --enable-indev="$(join "${indevs[@]}")" \
  --enable-bsfs \
  --enable-hwaccel="$(join "${hwaccels[@]}")" | tee configure.out
# configure only warns about a name it doesn't know, and a library it can't find just turns a
# component off; both are mistakes here.
if grep -q 'did not match anything' configure.out; then
  grep 'did not match anything' configure.out >&2; exit 1
fi
grep -q '^License: LGPL version 2.1 or later$' configure.out ||
  { echo "not an LGPL v2.1+ build" >&2; exit 1; }
for name in SECURETRANSPORT ZLIB HTTPS_PROTOCOL AVFOUNDATION_INDEV LIBDAV1D_DECODER \
    LIBMP3LAME_ENCODER H264_VIDEOTOOLBOX_ENCODER HEVC_VIDEOTOOLBOX_ENCODER AAC_AT_ENCODER \
    H264_VIDEOTOOLBOX_HWACCEL AV1_VIDEOTOOLBOX_HWACCEL PRORES_VIDEOTOOLBOX_HWACCEL; do
  grep -q "^#define CONFIG_$name 1$" config.h config_components.h ||
    { echo "configure left out $name" >&2; exit 1; }
done
# The home folder is cut out of the configure line the libraries keep (avcodec_configuration()),
# so the app doesn't carry it, as scripts/bundle-macos.sh does for Rust's source paths.
sed -i '' "s|$HOME|~|g" config.h
make -j"$jobs"
make install

# Every dylib: built for macOS 13, named by @rpath, and needing nothing but macOS and each other.
for lib in "$PREFIX"/lib/lib*.*.dylib; do
  [ -L "$lib" ] && continue
  minos="$(otool -l "$lib" | awk '/LC_BUILD_VERSION/ {b = 1} b && /minos/ {print $2; exit}')"
  [ "$minos" = 13.0 ] || { echo "$lib is for macOS $minos" >&2; exit 1; }
  otool -D "$lib" | tail -n +2 | grep -q '^@rpath/' || { echo "$lib isn't named by @rpath" >&2; exit 1; }
  if otool -L "$lib" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/|@rpath/)'; then
    echo "$lib needs a library macOS doesn't have" >&2; exit 1
  fi
  if strings "$lib" | grep -qF "$HOME"; then echo "$lib holds the home folder's path" >&2; exit 1; fi
  echo "$(basename "$lib") $(du -k "$lib" | cut -f1) KB, macOS $minos"
done
