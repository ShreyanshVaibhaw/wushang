# Third-party notices

wushang's own code is under the MIT licence in [LICENSE](LICENSE).
It ships with, or contains, the work below, each under its own licence.
The full licence texts are in [licenses](licenses).

## Libraries in wushang.app/Contents/Frameworks

These are loaded at run time, so you can swap any of them for your own build of the same version.
[scripts/build-ffmpeg-macos.sh](scripts/build-ffmpeg-macos.sh) and [scripts/build-libass-macos.sh](scripts/build-libass-macos.sh) rebuild them from the release tarballs listed here, unmodified.

| Library | Version | Licence | In | Source |
|---|---|---|---|---|
| FFmpeg | 9.0.2 | LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)) | `libav*.dylib`, `libsw*.dylib` | https://ffmpeg.org/releases/ |
| dav1d | 1.5.4 | BSD 2-Clause ([text](licenses/dav1d.txt)) | `libavcodec.63.dylib` | https://download.videolan.org/pub/videolan/dav1d/ |
| LAME | 4.0 | LGPL 2 or later, used here under LGPL 2.1 ([text](licenses/LGPL-2.1.txt)) | `libavcodec.63.dylib` | https://sourceforge.net/projects/lame/files/lame/ |
| libass | 0.17.5 | ISC ([text](licenses/libass.txt)) | `libass.9.dylib` | https://github.com/libass/libass/releases |
| FreeType | 2.15.0 | FreeType License ([text](licenses/FreeType-FTL.txt)) | `libass.9.dylib` | https://download.savannah.gnu.org/releases/freetype/ |
| FriBidi | 1.0.18 | LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)) | `libass.9.dylib` | https://github.com/fribidi/fribidi/releases |
| HarfBuzz | 15.0.0 | Old MIT ([text](licenses/HarfBuzz.txt)) | `libass.9.dylib` | https://github.com/harfbuzz/harfbuzz/releases |

FFmpeg is built without `--enable-gpl`, `--enable-version3` or `--enable-nonfree`, so it stays LGPL.
It holds libavcodec, libavformat, libavdevice, libavutil, libswresample and libswscale, with the demuxers, decoders, parsers, muxers, encoders and protocols the build script lists, and uses macOS's own VideoToolbox, AudioToolbox, AVFoundation, Secure Transport and zlib.

Portions of this software are copyright © 2026 The FreeType Project (https://freetype.org).
All rights reserved.

## Rust crates compiled into the wushang executable

[licenses/rust-crates.txt](licenses/rust-crates.txt) lists all of them with their versions and licences, followed by the licence files of those whose licences ask for them to go along.
[scripts/crate-licenses.py](scripts/crate-licenses.py) makes it from Cargo.lock.

| Crates | Licence |
|---|---|
| cpal, and the crates that offer Apache 2.0 among other licences (the `objc2-*` framework crates, `image`, `resvg`, `ureq`, `rustls`, `bitflags`, `libc`, ...) | Apache 2.0, used here under it ([text](licenses/Apache-2.0.txt)) |
| objc2, block2, bytes, fontdb, harfrust, rgb and other MIT-only crates | MIT (texts in [rust-crates.txt](licenses/rust-crates.txt)) |
| tiny-skia, subtle, arrayref, libloading, rustls-webpki, untrusted, slotmap, zlib-rs and the other BSD, ISC and zlib-licensed crates | their own licences (texts in [rust-crates.txt](licenses/rust-crates.txt)) |
| ring | Apache 2.0 and ISC, with BoringSSL's licences (texts in [rust-crates.txt](licenses/rust-crates.txt)) |
| encoding_rs | MIT or Apache 2.0, plus BSD 3-Clause for its WHATWG data (texts in [rust-crates.txt](licenses/rust-crates.txt)) |
| webpki-roots | CDLA Permissive 2.0, for Mozilla's root certificates (text in [rust-crates.txt](licenses/rust-crates.txt)) |
| ffmpeg-next, ffmpeg-sys-next | WTFPL |

## Design, font and data

- The interface and the icons in [assets/icons](assets/icons), except the logo, come from [VLC Redesign](https://www.figma.com/community/file/1691465999134370980/vlc-redesign) by Juxtopposed, under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
  `volume_fill.svg` and `sidebar.svg` were redrawn into equivalent shapes.
- The Inter font in [assets/fonts](assets/fonts) is by The Inter Project Authors, under the SIL Open Font License 1.1 ([OFL.txt](assets/fonts/OFL.txt)).
- The 18 equalizer presets in [src/dsp.rs](src/dsp.rs) are VLC's, from `modules/audio_filter/equalizer_presets.h`, copyright VideoLAN and VLC authors, under LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)).

wushang is not affiliated with or endorsed by VideoLAN or Juxtopposed.
VLC is a trademark of VideoLAN.
