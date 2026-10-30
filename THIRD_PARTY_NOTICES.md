# Third-party notices

wushang's own code is under the MIT licence in [LICENSE](LICENSE).
It ships with, or contains, the work below, each under its own licence.
The full licence texts are in [licenses](licenses).

## Libraries in the DLLs next to wushang.exe

These are loaded at run time, so you can swap any of them for your own build of the same version.
[scripts/build-ffmpeg.sh](scripts/build-ffmpeg.sh) and [scripts/build-libass.sh](scripts/build-libass.sh) rebuild them from the release tarballs listed here, unmodified.

| Library | Version | Licence | In | Source |
|---|---|---|---|---|
| FFmpeg | 9.0.2 | LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)) | `av*.dll`, `sw*.dll` | https://ffmpeg.org/releases/ |
| libbluray | 1.3.4 | LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)) | `avformat-63.dll` | https://download.videolan.org/pub/videolan/libbluray/ |
| dav1d | 1.5.4 | BSD 2-Clause ([text](licenses/dav1d.txt)) | `avcodec-63.dll` | https://download.videolan.org/pub/videolan/dav1d/ |
| zlib | 1.3.1 | zlib ([text](licenses/zlib.txt)) | `avcodec-63.dll`, `avformat-63.dll` | https://zlib.net/ |
| libass | 0.17.3 | ISC ([text](licenses/libass.txt)) | `libass-9.dll` | https://github.com/libass/libass/releases |
| FreeType | 2.13.3 | FreeType License ([text](licenses/FreeType-FTL.txt)) | `libass-9.dll` | https://download.savannah.gnu.org/releases/freetype/ |
| FriBidi | 1.0.16 | LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)) | `libass-9.dll` | https://github.com/fribidi/fribidi/releases |
| HarfBuzz | 10.0.1 | Old MIT ([text](licenses/HarfBuzz.txt)) | `libass-9.dll` | https://github.com/harfbuzz/harfbuzz/releases |

Portions of this software are copyright © 2024 The FreeType Project (www.freetype.org).
All rights reserved.

## Rust crates compiled into wushang.exe

| Crate | Licence |
|---|---|
| cpal, dasp_sample | Apache 2.0 ([text](licenses/Apache-2.0.txt)) |
| windows, windows-core, windows-numerics and the other `windows-*` crates, bitflags, libc | MIT or Apache 2.0, used here under Apache 2.0 ([text](licenses/Apache-2.0.txt)) |
| ffmpeg-next, ffmpeg-sys-next | WTFPL |

`cargo tree -e normal` lists them with their versions.

## Design, font and data

- The interface and the icons in [assets/icons](assets/icons), except the logo, come from [VLC Redesign](https://www.figma.com/community/file/1691465999134370980/vlc-redesign) by Juxtopposed, under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
  `volume_fill.svg` and `sidebar.svg` were redrawn into equivalent shapes Direct2D can render.
- The Inter font in [assets/fonts](assets/fonts) is by The Inter Project Authors, under the SIL Open Font License 1.1 ([OFL.txt](assets/fonts/OFL.txt)).
- The 18 equalizer presets in [src/dsp.rs](src/dsp.rs) are VLC's, from `modules/audio_filter/equalizer_presets.h`, copyright VideoLAN and VLC authors, under LGPL 2.1 or later ([text](licenses/LGPL-2.1.txt)).

wushang is not affiliated with or endorsed by VideoLAN or Juxtopposed.
VLC is a trademark of VideoLAN.
