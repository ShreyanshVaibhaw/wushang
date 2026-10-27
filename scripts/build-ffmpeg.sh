#!/usr/bin/env bash
# Cross-compiles a slim LGPL FFmpeg for myvlc on Linux/WSL with MinGW-w64.
# Only the demuxers, parsers and decoders a desktop player needs are built, plus D3D11VA, dav1d
# for software AV1, and zlib for Matroska tracks stored compressed. Subtitles cover embedded text
# and picture tracks and the common external files (.srt .ass .ssa .vtt .sub .idx .smi .sup).
# Networking covers the stream protocols a player opens (http/https/HLS/RTSP/UDP/RTP); TLS comes
# from Windows Schannel, so https needs no extra library. The DASH demuxer stays out: it needs
# libxml2, and HLS/RTSP cover the common live formats.
#
# Needs: gcc-mingw-w64-x86-64-win32 nasm make pkg-config meson ninja-build, plus gcc and
# libc6-dev for FFmpeg's build-time host tools.
# Usage: SRC=/path/to/ffmpeg-9.0.2 DAV1D_SRC=/path/to/dav1d-1.5.4 PREFIX=/path/to/output \
#        ZLIB_SRC=/path/to/zlib-1.3.1 scripts/build-ffmpeg.sh
set -euo pipefail

SRC=${SRC:-/mnt/d/tools/src/ffmpeg-9.0.2}
DAV1D_SRC=${DAV1D_SRC:-/mnt/d/tools/src/dav1d-1.5.4}
ZLIB_SRC=${ZLIB_SRC:-/mnt/d/tools/src/zlib-1.3.1}
PREFIX=${PREFIX:-/mnt/d/tools/ffmpeg-9.0.2-myvlc}
# Static libraries linked into the FFmpeg DLLs: none of them ships as a DLL of its own.
DAV1D_PREFIX="$PREFIX-deps"

# zlib (zlib licence) inflates Matroska tracks stored with zlib compression, which anime releases
# often use for their ASS subtitles; without it the matroska demuxer can't read those tracks.
make -C "$ZLIB_SRC" -f win32/Makefile.gcc clean
make -C "$ZLIB_SRC" -f win32/Makefile.gcc -j"$(nproc)" PREFIX=x86_64-w64-mingw32- libz.a
make -C "$ZLIB_SRC" -f win32/Makefile.gcc install PREFIX=x86_64-w64-mingw32- SHARED_MODE=0 \
  INCLUDE_PATH="$DAV1D_PREFIX/include" LIBRARY_PATH="$DAV1D_PREFIX/lib" \
  BINARY_PATH="$DAV1D_PREFIX/bin"

# dav1d (BSD-2) decodes AV1 on GPUs that can't. It is built as a static library and linked into
# avcodec, so no extra DLL ships.
rm -rf "$DAV1D_SRC/build-mingw"
meson setup "$DAV1D_SRC/build-mingw" "$DAV1D_SRC" \
  --cross-file="$DAV1D_SRC/package/crossfiles/x86_64-w64-mingw32.meson" \
  --prefix="$DAV1D_PREFIX" --libdir=lib \
  --buildtype=release --default-library=static \
  -Denable_tools=false -Denable_tests=false
ninja -C "$DAV1D_SRC/build-mingw" install
# Only dav1d's .pc file, never the build machine's, may answer FFmpeg's pkg-config checks. That
# makes the plain pkg-config safe to use; configure would otherwise look for a cross-prefixed one.
export PKG_CONFIG_LIBDIR="$DAV1D_PREFIX/lib/pkgconfig"

join() { local IFS=,; echo "$*"; }

demuxers=(mov matroska avi mpegts mpegps flv ogg asf ivf obu
          wav w64 aiff caf mp3 aac ac3 eac3 dts truehd flac
          h264 hevc m4v mpegvideo
          srt ass webvtt microdvd subviewer sami vobsub sup
          hls rtsp sdp)
protocols=(file data http https tls tcp udp rtp crypto)
parsers=(h264 hevc av1 vp8 vp9 mpeg4video mpegvideo vc1 mjpeg
         aac aac_latm ac3 mpegaudio opus vorbis flac dca mlp
         dvdsub dvbsub)
decoders=(h264 hevc av1 libdav1d vp8 vp9 mpeg4 mpeg2video mpeg1video vc1 wmv3 msmpeg4v3 mjpeg theora
          aac aac_latm mp3float mp2float ac3 eac3 opus vorbis flac alac dca truehd wmav2
          pcm_s16le pcm_s16be pcm_s24le pcm_s32le pcm_f32le pcm_u8 pcm_alaw pcm_mulaw pcm_bluray pcm_dvd
          subrip srt ass ssa webvtt movtext text microdvd subviewer sami dvdsub dvbsub pgssub)
# myvlc uses the *_d3d11va2 hwaccels, which decode into AV_PIX_FMT_D3D11 textures it presents
# without copying. Their code sits in the same objects as the older *_d3d11va ones, and the
# Makefile only builds those objects when *_d3d11va is enabled, so both families are listed.
hwaccels=()
for codec in h264 hevc av1 vp9 mpeg2 vc1 wmv3; do hwaccels+=("${codec}_d3d11va" "${codec}_d3d11va2"); done

cd "$SRC"
./configure \
  --prefix="$PREFIX" \
  --target-os=mingw32 --arch=x86_64 --cross-prefix=x86_64-w64-mingw32- \
  --enable-shared --disable-static \
  --disable-programs --disable-doc --disable-debug \
  --disable-avdevice --disable-avfilter \
  --disable-autodetect --enable-w32threads --enable-d3d11va \
  --enable-network --enable-schannel \
  --enable-libdav1d --pkg-config=pkg-config --pkg-config-flags=--static \
  --enable-zlib --extra-cflags="-I$DAV1D_PREFIX/include" \
  --disable-everything \
  --enable-protocol="$(join "${protocols[@]}")" \
  --enable-demuxer="$(join "${demuxers[@]}")" \
  --enable-parser="$(join "${parsers[@]}")" \
  --enable-decoder="$(join "${decoders[@]}")" \
  --enable-hwaccel="$(join "${hwaccels[@]}")" \
  --extra-ldflags="-static-libgcc -L$DAV1D_PREFIX/lib" \
  --ln_s="cp -f" # D: is exFAT, which has no symlinks
make -j"$(nproc)"
make install
# MSVC import libraries land next to the DLLs; ffmpeg-sys-next looks for them in lib/.
mv -f "$PREFIX"/bin/*.lib "$PREFIX"/lib/
