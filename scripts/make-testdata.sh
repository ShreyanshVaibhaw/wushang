#!/usr/bin/env bash
# Generates the test clips in testdata/ that are too big to keep in git: 1080p30 test patterns in
# each codec myvlc decodes. The small fixtures with exact subtitle timings (test_tracks.mkv,
# test_subs.mkv, tracks.srt) are committed instead.
#
# Needs an ffmpeg with libx264, libx265, libvpx-vp9, libsvtav1 and libopus (a GPL build, e.g.
# `winget install yt-dlp.FFmpeg`). Set FFMPEG to its path if it isn't on PATH.
#
#   bash scripts/make-testdata.sh
set -euo pipefail

ff="${FFMPEG:-ffmpeg}"
out="$(dirname "$0")/../testdata"
mkdir -p "$out"

clip() { "$ff" -v error -y "$@"; }
pattern() { echo "-f lavfi -i $1=size=1920x1080:rate=30:duration=$2"; }
tone() { echo "-f lavfi -i sine=frequency=440:duration=$1:sample_rate=$2"; }

# 44.1 kHz audio on purpose: it exercises resampling to the device's 48 kHz.
clip $(pattern testsrc 6) $(tone 6 44100) -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac -shortest "$out/test_h264.mp4"
clip $(pattern testsrc2 60) $(tone 60 44100) -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac -shortest "$out/test_60s.mp4"
clip $(pattern testsrc2 60) -c:v libx264 -preset veryfast -pix_fmt yuv420p "$out/test_60s_noaudio.mp4"
clip $(pattern testsrc 6) $(tone 6 48000) -c:v libx265 -preset veryfast -pix_fmt yuv420p -c:a libopus -shortest "$out/test_hevc.mkv"
clip $(pattern testsrc 6) $(tone 6 48000) -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 2M -pix_fmt yuv420p -c:a libopus -shortest "$out/test_vp9.webm"
clip $(pattern testsrc 6) $(tone 6 48000) -c:v libsvtav1 -preset 11 -svtav1-params log-level=0 -pix_fmt yuv420p -c:a libopus -shortest "$out/test_av1.mkv"
echo "testdata ready in $out"
