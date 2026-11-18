<p align="center"><img src="assets/wushang.svg" width="112" alt="wushang logo"></p>

# wushang

wushang is a desktop media player for Windows and macOS, written in Rust.
It plays video, music, and network streams, and keeps a library of the folders, playlists, and playback positions you want to come back to.
The interface is based on Juxtopposed's VLC redesign.

## Download

Get the installer or portable ZIP for your platform from [GitHub Releases](https://github.com/ShreyanshVaibhaw/wushang/releases/latest).
You don't need to install FFmpeg separately.

| Platform | Requirements | Install | Portable |
| --- | --- | --- | --- |
| Windows | Windows 10 or 11, x64 | `wushang-<version>-windows-x64.msi` | `wushang-<version>-windows-x64.zip` |
| macOS | macOS 13 or later, Apple Silicon (M-series) | `wushang-<version>-macos-arm64.dmg` | `wushang-<version>-macos-arm64.zip` |

**Windows.** Open the MSI to install for your account without admin rights.
For the portable copy, extract the ZIP and run `wushang.exe` from that folder.
To make wushang your default player, open it once, then use Settings > Apps > Default apps.

**macOS.** Open the DMG and drag `wushang.app` into Applications, or extract the ZIP to get the same app.
The release is signed ad hoc and is not notarized by Apple.
If macOS blocks the app you downloaded from this repository, try opening it once, then look for **Open Anyway** in System Settings > Privacy & Security.
To make wushang the default for a file type, select a file in Finder, choose File > Get Info, select wushang under Open with, and click Change All.

Both apps can check GitHub for updates at startup.
On Windows, the installed copy hands updates to Windows Installer and the portable copy downloads a replacement ZIP for you to extract.
On macOS, quit wushang before replacing it in Applications with the copy from the new DMG or ZIP.
Updates keep your library, settings, and playback history.

## What you can do

- **Playback tabs.** Open up to eight tabs, each with its own queue, position, tracks, picture settings, volume, and audio effects.
  Background tabs keep playing silently; only the selected tab is audible.
- **Media library.** Add video and music folders, search, mark favourites, resume from Recent, and create or export M3U playlists.
- **Player controls.** Preview frames while seeking, use the mini player, rearrange the control bar, and choose audio and subtitle languages per folder or playlist.
- **Subtitles and audio.** Switch tracks, load external subtitles, render styled ASS/SSA with libass, and adjust playback speed, equalizer, or audio delay.
- **Streams and discs.** Play HTTP(S), HLS, RTSP, UDP, and RTP streams, subscribe to RSS or Atom podcasts, or browse internet radio.
  wushang also plays the main title of an unencrypted DVD or Blu-ray, without disc menus.
- **Recording and conversion.** Take snapshots, record media or a capture device, convert files to MP4 or MP3, and serve compatible media over HTTP to other players on your local network.

| Action | Windows | macOS |
| --- | --- | --- |
| Open a file | `Ctrl+O` | `⌘O` |
| Open a network stream | `Ctrl+N` | `⌘N` |
| Open / close playback tab | `Ctrl+T` / `Ctrl+W` | `⌘T` / `⌘W` |
| Select tab 1 to 8 / last tab | `Ctrl+1` to `Ctrl+8` / `Ctrl+9` | `⌘1` to `⌘8` / `⌘9` |
| Toggle mini player | `Ctrl+M` | `⌘M` |

On macOS, opening media from Finder, Open With, or the Dock while wushang is running opens a new tab.
On Windows, use **Play in New Tab** inside the app; Explorer does not forward files to a running copy.

## What changed in 1.0.5

- Both apps now have playback tabs and keep video playing while the file picker is open.
- Library scans and saved-state writes run off the UI thread, and Customize shows the controls that don't fit in the current window.
- Windows improves UTF-16 subtitle detection, keeps attached fonts separate between playback sessions, and fixes audio delay affecting seek and resume positions.
- macOS skips unreadable queue entries, accepts dropped subtitle files, improves conversion sync and cleanup, and supports input methods in library search.

See the [1.0.5 release notes](https://github.com/ShreyanshVaibhaw/wushang/releases/tag/v1.0.5) for the full list and platform-specific limits.

## Build from source

This repository keeps the Windows app on `main` and the macOS app on [`macos`](https://github.com/ShreyanshVaibhaw/wushang/tree/macos).
Clone the branch for the platform you're building on.

### Windows

Use Rust 1.88 or newer with the MSVC toolchain, an FFmpeg 9 shared build with headers and import libraries, and libclang.
Set `FFMPEG_DIR` and `LIBCLANG_PATH` in [`.cargo/config.toml`](.cargo/config.toml) to match your machine.
Set `LIBASS_DIR` as well if you have a libass build for styled subtitles.

```powershell
git clone --branch main https://github.com/ShreyanshVaibhaw/wushang.git wushang-windows
cd wushang-windows
cargo run --release -- 'C:\path\to\video.mp4'
```

The [Windows guide](WINDOWS.md) covers dependency builds, WiX packaging, playback settings, implementation details, and benchmarks.

### macOS

Use an Apple Silicon Mac with Apple's Command Line Tools and Rust 1.88 or newer.
If the tools aren't installed, run `xcode-select --install` first.
For a local development build with Homebrew:

```sh
git clone --branch macos https://github.com/ShreyanshVaibhaw/wushang.git wushang-macos
cd wushang-macos
brew install rust ffmpeg libass pkgconf
cargo run --release -- /path/to/video.mp4
```

`cargo run` launches a development binary, not a Finder-ready `.app`.
The [macOS guide](https://github.com/ShreyanshVaibhaw/wushang/blob/macos/README.md) covers playback, native Mac behavior, and known limits.
For a distributable app, follow its [build and packaging steps](https://github.com/ShreyanshVaibhaw/wushang/blob/macos/README.md#build-requirements) to build the slim LGPL FFmpeg and libass libraries and run `scripts/bundle-macos.sh`.
Homebrew's FFmpeg is for local development, not for bundling.

## How it works

Both apps use FFmpeg for demuxing and decoding, with hardware decoding where supported and a software fallback.
Windows uses D3D11VA for hardware decoding, Direct3D 11 for video output, and WASAPI for audio.
macOS uses VideoToolbox for hardware decoding, Metal for video output, and Core Audio for audio.
The Mac app uses AppKit for the window and native fullscreen, AVFoundation for capture, and Extended Dynamic Range (EDR) for HDR output on supported displays.

## License and credits

wushang is available under the [MIT license](LICENSE).
See [third-party notices](THIRD_PARTY_NOTICES.md) for the licenses of the libraries, fonts, and other assets it uses.
The interface and most icons come from Juxtopposed's [VLC Redesign](https://www.figma.com/community/file/1691465999134370980/vlc-redesign), licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
wushang is not affiliated with Juxtopposed or VideoLAN.
