# myvlc

A small Windows video player in Rust, built with the same pipeline as VLC but without its plugin system.
FFmpeg demuxes and decodes, the GPU decodes and presents through Direct3D 11, and WASAPI plays the audio.
The interface follows Juxtopposed's VLC redesign (see Credits).
[VLC_ANALYSIS.md](VLC_ANALYSIS.md) explains the VLC internals this design comes from.

## Run

```bash
cargo run --release -- path\to\video.mp4
```

Without a file, myvlc opens on the library.
The first time it does (no `settings.txt` yet), the design's welcome shows over it in three steps.
The first shows the media folder the library starts from, and Browse adds another to Videos.
The second picks the theme and accent colour, which apply as you click, over a small picture of the player.
The third lists the basic keyboard shortcuts; Finish, or Esc at any step, closes it.
`--sw` forces software decoding, which is useful for comparing the two paths.
`--play-and-exit` plays the file from the start and quits at its end, like VLC's option of the same name; the measurement scripts use it.

### Library

The library page follows the redesign's library frames.
The sidebar has Home, Videos, Music, Network, Playlists and Recent, plus "Add item", which adds a folder through the system folder picker.
On first run Videos and Music start with your Windows Videos and Music folders.
Folders open in place, with back and forward arrows and a breadcrumb that shortens to "..." when the window is narrow.
The header shows the item count, the sort menu (unwatched or watched first, newest or oldest, title, artist or album; the orange dot means it isn't title A-Z), grid or list view, and a search box that filters the page as you type.
Thumbnails come from a frame at 10% of each video, and audio files show their embedded cover.
A bar under a thumbnail shows how much of it you watched, and finished files are dimmed.
Music also has Songs, Albums, Artists and Genres, built from the files' tags.
The tags are kept in `tags.txt` next to the library state, so these fill in at once on the next start; only new or changed files are read again.
Home collects Continue Watching, Listen Again, and your most visited folders, playlists and artists.

Clicking a file plays it, resuming where you left it.
At the end it moves on to the next file of the page it came from, as VLC's playlist does.
Esc or the folder name in the player's title bar goes back to the library, and the video keeps playing in the mini player at the bottom right; click it to return.

In the player, the sidebar button opens the design's overlay panel over the video: a rail of section icons and a compact list of the same library, with history, breadcrumb and sort.
Clicking a file there plays it with the panel still open; a click on the video or Esc closes it, and the arrow button at its top right opens the full library page.
Dropping a file on the window plays it, or adds it to the playlist on screen.
"Create playlist" and the header's + open the New Playlist dialog from the design: a name, an optional thumbnail picture, and files picked or dropped on the window.
On a playlist's page the header's + adds files to it.
Playlists are standard M3U files.

Right-click a file, folder or playlist (on the library page or in the player's sidebar panel) for the design's context menus:
- files: play, mark as watched, favourite, add to the queue or a playlist, show in File Explorer, go to its folder, add a subtitle file, properties, and remove from the playlist when on a playlist's page;
- folders: add files or a folder, rescan, info;
- playlists: play, shuffle, favourite, export as M3U, add to the queue, rename (Properties & Metadata), delete.
Favourites appear first on Home.

Library state (folders, recent files and positions, visit and play counts) lives in `%APPDATA%\myvlc`, or in the folder named by the `MYVLC_HOME` environment variable, which never seeds your own folders and suits tests.

### Player

The player has the redesign's own title bar and control bar.
Click play/pause, previous (restart, or the previous file within the first 3 seconds), next, the seek bar, the volume wedge (up to 200%, like VLC) and the speaker (mute).
The title bar has minimize, maximize, fullscreen and close, and dragging it moves the window.
While playing, the bars hide after 3 seconds without mouse movement and the video fills the window inside a 10 px rounded frame; moving the mouse brings them back.
The "…" button opens the main menu from the design: open files, open a folder, recent files, settings profiles, keyboard shortcuts, Customize Player, Settings and About.
Right-clicking the video opens the player menu: play, stop, previous, next, hide the controls, the mini player, and submenus for speed and sleep timer, video tracks and transforms, audio tracks, stereo mode and output device, subtitles, the settings tabs, the queue, and opening media.
Items for features myvlc doesn't have (discs, capture devices, converting, recording) are shown dimmed.

The playlist button opens the queue panel from the design, in the settings panel's place, with three tabs:

- Queue: the files previous and next step through, with their lengths and the total.
  The playing file is orange with its progress underneath, and files watched to the end are dimmed.
  Click a file to play it, drag it to reorder, and use sort (by name), repeat (off, all, one), shuffle, minus (removes the file last clicked) and + (adds files).
  Repeat One replays the file when it ends, while next and previous still move on; shuffle plays every file once before Repeat All wraps.
  A file's "…" or a right-click on it opens the design's file menu, where Play plays that row, Play Next queues it after the playing file (shuffled or not), and Remove from Queue takes it out.
- Bookmarks: positions in the playing file; + adds one where it is, a click seeks there.
  They live in `bookmarks.txt` next to the library state.
- Chapters: the file's chapters with their titles; a click seeks there.

The audio and subtitle buttons open the track popup from the design: video, audio and subtitle tracks (with names like "English" from the file's language tags), the output device, and external subtitle files, with + to load one by hand.
"Add Audio File..." in the player menu's Audio submenu plays another file's sound with the video, like a dub or commentary kept beside it; it joins the audio track list under its file name and follows every seek.
A subtitle file with the same name as the video, next to it or in a "Subs" folder, loads on its own, as in VLC.
Old subtitle files in a Windows code page instead of UTF-8 are read in the right one: a language tag such as `film.ru.srt` names it, and otherwise myvlc tries the Western, Central European, Cyrillic, Greek, Turkish, Hebrew, Arabic, Baltic, Japanese, Chinese and Korean code pages and keeps the one whose text reads like real words.
The gear opens the settings panel, with four tabs:

- Playback: speed from 0.25x to 2x without changing pitch, a sleep timer, and playback statistics over the video.
- Video: flip, rotation, zoom, aspect ratio, crop, and brightness, contrast, hue and saturation (under "Expand Settings"), all done by the GPU's video processor.
- Audio: track, stereo mode (stereo, mono, left, right, reverse), audio delay, and VLC's 10-band equalizer with its 18 presets and the "2 Pass" option.
- Subtitles: track, subtitle delay and text size.

Items the design shows that myvlc can't do yet (HDR, resolution, render, reverse, audio visualizer, subtitle font and colours, presets) are drawn dimmed.
"Open Advanced Settings" opens the Settings window.
Switching tracks and seeking land on the exact moment: the decoders read from the keyframe before it and drop what comes earlier.

| Key | Action |
|---|---|
| Space | Pause and resume |
| ← / → | Skip back or forward: 10 s, Alt 3 s, Shift 1 min, Ctrl 5 min (all set in Settings) |
| ↑ / ↓, mouse wheel | Volume, 10% a press (the wheel scrolls in the library) |
| M | Mute |
| F or double-click the video | Borderless fullscreen |
| Esc | Close a menu, panel or dialog, leave fullscreen, or go to the library |
| Backspace | Back, in the library |
| S / P / N | Stop, previous file, next file |
| Ctrl+O / Ctrl+F | Open files / open a folder |
| Ctrl+P | Settings |
| Ctrl+\ | Hide the controls until the mouse moves |
| Ctrl+M | Mini player, and back |
| Ctrl+I | Properties of the file playing, or of the file under the pointer |
| F1 / Shift+F1 | Keyboard shortcuts / About |
| Q, Ctrl+Q | Quit |

The window opens the way it was launched, so a shortcut set to "Run: Minimized", or a script that asks for minimized without activation, never takes focus.

### Snapshots

Shift+S, "Take Snapshot" in the player menu's Video submenu, or the snapshot button from Customize Player saves the frame on screen as a PNG in `Pictures\myvlc`.
It is the full-resolution frame with flips, rotation, crop, aspect ratio and colour adjustments as shown, but not zoom or the interface.
The file is named after the video and the moment in it, such as `Holiday 1m05s.png`, and a line over the video says where it went.

### Customize Player

"Customize Player" in the "…" menu or the player's right-click menu opens the design's palette over the player, and every item and group in the bars gets a dashed box.
Drag an item from the palette into the title bar group or the control bar's left, centre or right group, between items or across groups, and drag one back onto the palette to take it out.
Dropping on the seek bar starts the centre group, and the control bar then takes two rows with the seek bar across the top, as in the design's customize frames.
Save keeps the bars in `toolbar.txt` next to the library state; Cancel or Esc drops the changes.

Besides the bars' own controls, the palette has spacers and dividers, snapshot, playback speed, skip 10 or 15 seconds either way, stop, repeat, shuffle, the mini player, zoom, quit, Settings, the library, the file name, properties, replay, chapters, favourite, add a bookmark, bookmarks and playlists.
Record, HDR, the right sidebar, a thumbnail and the artist and playlist names are dimmed, as myvlc can't do them yet.

### Mini player

The mini player is the design's "Miniplayer" card: the video, its progress bar, the file name and folder, and a queue button.
It shows at the library's bottom right while something plays, and Ctrl+M (or "Mini Player" in the player's right-click menu) turns the whole window into it.
The mini player window stays above other windows and opens at the bottom right of the screen, or where you last left it.
Drag it by its body to move it.

With the pointer on the card, the video dims and shows the design's controls: previous, play/pause and next in the middle, close at the top left and expand at the top right.
Close stops playback; expand goes back to the full player, and in the window it restores the window's old size and place.
The progress bar seeks with a click or a drag.
The queue button opens the queue above the video, in the design's style, with sort, remove and add; the window grows upward to make room.
Esc or Ctrl+M also leaves the mini player window.

### Network

Ctrl+N, "Open Network Stream" in the "..." menu, or "Add Network Stream" on the Network page asks for an address and plays it: http, https, HLS (`.m3u8`), RTSP, UDP or RTP.
A bare host like `example.com/live.m3u8` gets `http://` in front, as in VLC, and `myvlc <address>` plays one from the command line.
The address stays on the Network page; right-clicking it offers Remove.
A stream connects in the background while the player says "Connecting to ...", and if it can't play, the corner says so before the library comes back.
A live stream, which has no end, shows "Live" in place of its length.
Podcasts takes an RSS or Atom feed and lists its episodes, newest as the feed orders them, under the feed's own title; right-clicking a podcast removes it.
Radio lists the 100 most voted stations of the community [Radio Browser](https://www.radio-browser.info) directory, with their genre and country.
Episodes and stations play with the name the page shows, and next and previous move through the page.
Feeds and the directory load over WinHTTP on a background thread, so the window never waits on them.

### Settings

Ctrl+P or Settings in the "..." menu opens the Settings window from the design's "Settings" frames: categories on the left, their options on the right, and a search box that finds an option in any category.
Every option it lists does something; the design's categories with nothing behind them yet (Recording, Extensions) are dimmed.
Changes apply at once and are kept in `settings.txt` next to the library state.

- General: dark, light or system theme, and the accent colour.
  The design has one light frame, and myvlc turns every screen light from it, so menus, panels and dialogs follow the same greys.
  System follows the Windows "app mode" setting and switches when it changes.
  The accent replaces the design's orange everywhere except the VLC cone and the volume meter.
  Its nine colours are the onboarding frames' bright ones; Settings dims all but the one picked, as the design does.
- Interface: keep the window above others, remember its size and position, and when and how fast the controls hide.
- Privacy: whether recently played files are saved, and a button that clears the history.
- Controls: the four skip lengths, what a click, a double-click and the wheel do on the video, and the keyboard shortcuts.
- Playback: resuming where you left off, starting paused, the default speed, the preferred audio language, repeat, shuffle, and quitting at the end of the queue.
- Video: start in fullscreen, keep the screen awake while a video plays, and the default aspect ratio.
- Audio: output device, startup volume (or the last one), the Up/Down volume step, the equalizer preset, two-pass and stereo mode.
  "Custom" is the last equalizer tuned by hand in the panel, kept while a preset plays.
- Subtitles: on or off, the preferred language, loading subtitle files on their own, and their size, weight, colour, opacity, outline and shadow.
- Library: show hidden files and folders.
- Network: reconnect when a stream drops, and how long a stalled connection may wait (15 seconds by default).
- Performance: turn off GPU decoding.

The equalizer, stereo mode, subtitle size, repeat and shuffle chosen in the panels are kept the same way.
Speed, aspect ratio, tracks and decoding are per-file defaults, so a change there applies from the next file.

### Profiles

A profile is a whole set of settings under a name.
The "…" menu's Profile row names the one in force, and its submenu, from the design's "Profiles" frame, lists Movies, Shows, Music and Work, then any you added, with "New" at the bottom.
Picking a profile applies its settings at once, theme and accent included, and myvlc starts in it next time.
A profile starts as a copy of the settings in force when it is first picked or made, and changes from then on stay with it.
The window's place and the volume are left alone, since they belong to the machine rather than the profile.
"New" asks for a name, then adds the profile and switches to it.
Each profile keeps its settings in `profiles\<name>\settings.txt`, and `profile.txt` names the one in force.

## Build requirements

- Rust 1.88 or newer with the MSVC toolchain.
- An FFmpeg 9 shared build with headers and import libraries.
  This project uses a slim LGPL build made by [scripts/build-ffmpeg.sh](scripts/build-ffmpeg.sh).
  Any full FFmpeg 9 shared build also works, for example `ffmpeg-n9.0-latest-win64-lgpl-shared-9.0` from github.com/BtbN/FFmpeg-Builds.
- `libclang.dll`, which `ffmpeg-sys-next` needs to generate bindings at build time.
  The one here comes from the `libclang` 18.1.1 wheel on PyPI.

[.cargo/config.toml](.cargo/config.toml) points `FFMPEG_DIR` and `LIBCLANG_PATH` at both folders under `D:\tools`.
Change those two paths on another machine.
[build.rs](build.rs) copies the FFmpeg DLLs next to the built exe.

### Building the slim FFmpeg

The script cross-compiles zlib 1.3.1, dav1d 1.5.4 and FFmpeg 9.0.2 for Windows from Linux or WSL with MinGW-w64.
It needs these Ubuntu packages: `gcc-mingw-w64-x86-64-win32 nasm make pkg-config meson ninja-build gcc libc6-dev`.

```bash
SRC=/mnt/d/tools/src/ffmpeg-9.0.2 DAV1D_SRC=/mnt/d/tools/src/dav1d-1.5.4 \
  ZLIB_SRC=/mnt/d/tools/src/zlib-1.3.1 PREFIX=/mnt/d/tools/ffmpeg-9.0.2-myvlc \
  bash scripts/build-ffmpeg.sh
```

It builds the file protocol and the network ones a player opens (http, https, tcp, udp, rtp, with TLS from Windows' Schannel), 36 demuxers (HLS and RTSP among them), 20 parsers, 50 decoders and the D3D11VA hwaccels, plus libswresample and libswscale.
The subtitle decoders (SRT, ASS/SSA, WebVTT, MP4 text, MicroDVD, SubViewer, SAMI, DVD, DVB and PGS) and their file demuxers add 78 KB.
zlib, linked statically into `avformat-63.dll` (43 KB more), lets the Matroska reader open tracks stored zlib-compressed, which mkvmerge does to subtitles by default; without it those files log "Unsupported encoding type" and their subtitles are lost.
One of the decoders is libdav1d, linked statically into `avcodec-63.dll`, so AV1 plays in software on GPUs without AV1 support.
Encoders, muxers, filters, devices and the command-line programs are left out.
Networking adds 286 KB to `avformat-63.dll`, which then also imports `WS2_32`, `Secur32`, `CRYPT32` and `ncrypt`.
x86 assembly stays on, so software decoding is as fast as in a full build.
Apart from that, the DLLs import only `KERNEL32`, `msvcrt` and `bcrypt`.

## How it works

```text
demux thread     libavformat reads packets and routes them to per-stream queues; it also seeks
video decoder    libavcodec + D3D11VA decodes into GPU textures (software fallback)
audio decoder    opens WASAPI, decodes, resamples to the device format, fills a 0.3 s ring
audio callback   plays the ring and sets the master clock (sample time minus device latency)
main thread      window, input, and the render loop: show a frame when its time comes, skip late ones,
                 then paint the interface over it with Direct2D
thumbnailers     two threads decode one still per visible file, newest request first
prober           one thread reads music tags for Songs, Albums, Artists and Genres
```

The window, the GPU device and the WASAPI device live for the whole run.
Opening a file starts a session ([src/session.rs](src/session.rs)): its demux and decoder threads, which stop when the next file opens.
The audio device opens once at startup, because opening it takes 1.7 s on the test laptop.
[src/library.rs](src/library.rs) lists folders, keeps recent files and positions, and reads and writes the M3U playlists; it doesn't touch FFmpeg.
[src/thumbs.rs](src/thumbs.rs) makes thumbnails and reads tags with FFmpeg in software, so it never competes with playback for the GPU decoder.
The library page ([src/ui/browse.rs](src/ui/browse.rs)) is painted inside a Direct2D layer masked to everything but the mini player's video, so the grid can sit under the card while the live video shows through.

| VLC module | Here | File |
|---|---|---|
| access, demux, packetizer | libavformat | [src/player.rs](src/player.rs) |
| decoder, hw decoder (d3d11va) | libavcodec with `hw_device_ctx` | [src/player.rs](src/player.rs) |
| vout display (direct3d11) | `ID3D11VideoContext::VideoProcessorBlt` into a flip swap chain | [src/render.rs](src/render.rs) |
| audio output (mmdevice) | cpal on WASAPI shared mode | [src/audio.rs](src/audio.rs) |
| audio filters (scaletempo, equalizer, mono/channel remap) | WSOLA speed, VLC's equalizer, stereo modes | [src/dsp.rs](src/dsp.rs) |
| subtitle decoders and renderer (subsdec, freetype) | libavcodec subtitle decoders, Direct2D captions | [src/subs.rs](src/subs.rs), [src/captions.rs](src/captions.rs) |
| video filters (transform, adjust, crop) | the D3D11 video processor's rotation, mirror and filters | [src/render.rs](src/render.rs) |
| clock, late-picture dropping | audio master clock, skip to the newest due frame | [src/main.rs](src/main.rs) |
| Qt interface | Direct2D + DirectWrite interface in a borderless Win32 window | [src/ui.rs](src/ui.rs), [src/ui/browse.rs](src/ui/browse.rs), [src/main.rs](src/main.rs) |
| media library (medialibrary) | folders, recent files, M3U playlists, thumbnails, tags | [src/library.rs](src/library.rs), [src/thumbs.rs](src/thumbs.rs) |

The decoder, the video processor and Direct2D share one `ID3D11Device`, which myvlc creates with BGRA support and hands to FFmpeg.
Each present draws the video into the rounded video area, then Direct2D paints the bars, icons and text into the same back buffer.
The icons are the design's SVGs, rendered by Direct2D's SVG support, and the text uses Inter, embedded in the exe.
Layout numbers come from the design's 1920x1080 frame in DIPs, so the interface scales with the screen's DPI.

Decoded frames stay in the decoder's texture array, and the GPU's video processor converts YUV to RGB and scales straight into the swap chain.
Nothing is copied back to system memory.

AV1 needs one extra decision, because libavcodec always prefers libdav1d, which never uses the GPU.
myvlc asks the GPU for an AV1 decoder profile first.
If it has one, FFmpeg's own AV1 decoder runs on D3D11VA like every other codec; if not, libdav1d decodes on every CPU core.

A stream the GPU turns down after all, such as 10-bit H.264, which no GPU decodes, or a 4:4:4 AV1, is reopened in software on every core (libdav1d for AV1) and decoded again from the last keyframe.
The decoder thread repacks software frames to NV12 with swscale, splitting each frame across the cores, so the render thread only copies them to the GPU.
On the test laptop a 4K60 10-bit H.264 clip went from 90 dropped frames of 300 to about 50; the rest is the CPU's limit for decoding and repacking at once.

HDR10 (PQ) video gets tone mapped to the SDR screen, because video processors either can't do it or wash it out (the test laptop's Intel one does).
The video processor only converts such a frame from YCbCr to RGB, still PQ-encoded in BT.2020, which every driver that supports HDR displays can do.
Direct2D's colour effects do the rest ([src/tonemap.rs](src/tonemap.rs)): PQ to linear light, a tone curve per channel, then sRGB.
The curve leaves everything up to three quarters of HDR's reference white (203 nits) as it is and rolls the highlights above it off into the rest of SDR's range, up to the stream's MaxCLL.
On test clips with a known answer, the result is within a few levels of 255 of the exact conversion, and a 4K60 HDR10 clip plays with 2 of 240 frames dropped.
BT.709 and BT.601 video is unchanged by this.

The render loop runs above normal priority, as VLC's video output thread does.
While the window is dragged or resized, Windows runs a message loop of its own until the mouse lets go; a timer then runs the render loop's passes from the window procedure, so the video keeps playing.
A pass that fails (a lost frame) is logged and skipped rather than closing the player.
At normal priority, libdav1d's bursts across all cores made it wake up about 40 ms late and skip frames.

## Measured against VLC 3.0.23

Same laptop (Intel UHD + GTX 1650 Ti, Windows 11), same 6-second 1080p30 clips, release build.
Each figure is the median of three runs.
VLC ran with `--intf=dummy`, so its Qt GUI was not loaded either.
myvlc used the slim FFmpeg build.
CPU time swings by about 100 ms between runs of the same setup, so treat smaller gaps as noise.
These figures predate the interface; drawing it adds about 19 MB of working set and 9 MB of private memory (H.264 measured minimized: 103 MB working set, 88 MB private).

| Clip | Player | Working set | GPU memory | Threads | CPU time |
|---|---|---|---|---|---|
| H.264 + AAC, MP4 | VLC | 282 MB | 215 MB | 27 | 734 ms |
| H.264 + AAC, MP4 | myvlc | 82 MB | 84 MB | 13 | 516 ms |
| HEVC + Opus, MKV | VLC | 298 MB | 222 MB | 30 | 734 ms |
| HEVC + Opus, MKV | myvlc | 89 MB | 101 MB | 13 | 438 ms |

GPU memory is the "GPU Process Memory" shared usage counter.
Dedicated usage was 0 MB for both, so both decoded on the Intel GPU, which keeps video surfaces in system RAM.

The first frame appears 180 to 200 ms after launch.
On this laptop, opening the WASAPI stream takes 1.7 s inside `IAudioClient::Initialize`, and VLC waits for it, which is the "Decoder wait done in 2514 ms" in its log.
myvlc opens audio on the audio thread while the window and first frame come up, and starts the clock once sound plays.

H.264 and HEVC played with no dropped frames in every run; VP9 dropped one frame in one of four runs.
A 60-second clip takes 62.3 s of wall time: 60 s of playback plus the 1.7 s audio device startup.

## Slim FFmpeg against the full build

The same `myvlc.exe` ran next to each set of DLLs; both are FFmpeg 9 with the same ABI.
Medians of three runs per clip.
The memory, CPU and startup rows were measured before libdav1d was added to the slim build.

| | Full (BtbN LGPL) | Slim, with libdav1d |
|---|---|---|
| Five FFmpeg DLLs on disk | 114.4 MB | 14.3 MB (12.1 MB without libdav1d) |
| `avcodec-63.dll` | 86.9 MB | 9.9 MB (7.9 MB without libdav1d) |
| H.264: private memory / working set | 88.8 / 88.2 MB | 79.1 / 82.2 MB |
| HEVC: private memory / working set | 98.2 / 94.9 MB | 88.6 / 89.1 MB |
| VP9: private memory / working set | 65.3 / 64.6 MB | 55.5 / 58.5 MB |
| CPU time, GPU decoding | 594-641 ms | 516-594 ms |
| CPU time, software HEVC (`--sw`) | 2,406 ms | 2,234 ms |
| First frame on screen | 189-196 ms | 188-199 ms |

Disk size drops by 88%.
Memory drops by about 10 MB of private bytes, because Windows maps DLL code lazily and only the full build's writable data pages cost RAM up front.
CPU time and startup are the same within run-to-run noise.

## AV1 in software

Neither GPU in this laptop decodes AV1, so AV1 goes to libdav1d.
These runs started myvlc minimized without activation, so they never took focus.
A minimized window skips display composition, so the CPU figures are a little lower than for a visible window and are not comparable with the tables above.
Peak memory comes from Windows' own per-process counters.

| 1080p30 clip, 6 s | AV1, libdav1d | H.264, GPU |
|---|---|---|
| CPU time | 2.8 s median of 10 runs (3.5 s over another 5) | 0.7 s median of 3 runs |
| Peak working set | 154 MB | 84 MB |
| Peak private memory | 169 MB | 79 MB |
| Dropped frames | 2 of 2,700 over 15 runs | 0 of 540 over 3 runs |

Before the render loop's priority was raised, the same AV1 clip dropped 5 of 900 frames over 5 runs.
The first frame still appears in about 0.2 to 0.6 s.
Opening the audio device took 1.2 to 3.5 s in these runs, and playback starts moving when it is ready.

## Known limits

Each of these is marked with a `ponytail:` comment in the code, along with the upgrade path.

- ASS and SSA subtitles show as plain centred text: positioning, colours, style-sheet italics and karaoke are ignored (libass would add them).
- Without a language tag, traditional Chinese (Big5) subtitles read as simplified (GBK), and Turkish or Baltic ones as Western, since the same bytes are valid in both.
- Contrast and saturation go up to 2x, like VLC's adjust filter; brightness and hue use the GPU's full range.
- HDR10 is always tone mapped to SDR, even on an HDR screen; there is no HDR output.
- SDR BT.2020 and HLG go through the video processor's own conversion, which on the test laptop's Intel GPU leaves saturated colours paler than they should be, and HLG's highlights clip.
- Software frames reach the GPU as 8-bit NV12, so 10-bit video loses its extra precision there, and 4K60 10-bit H.264 drops frames on a laptop CPU.
- Audio devices must mix in f32, which every WASAPI shared-mode device seen so far does.
- The audio callback takes a mutex.
- A dropped stream reconnects only where FFmpeg does it (http and HLS); RTSP and UDP end when the connection does.
- A podcast feed is fetched once per run, so new episodes show after a restart.
- A video stream that fails its first 60 packets counts as undecodable, and playback goes on with audio only.
- Thumbnails stay in memory for the whole run, about 350 KB each.
- The folder picker is modal, so the video pauses on screen while it is open (audio keeps playing).

## Licence of the build

Both FFmpeg builds are LGPL, so this player can use any licence as long as FFmpeg stays dynamically linked.
A GPL FFmpeg build would make the player GPL.

## Credits

The interface is based on [VLC Redesign](https://www.figma.com/community/file/1691465999134370980/vlc-redesign) by Juxtopposed, shared on the Figma Community under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
The icons in [assets/icons](assets/icons) are exported from that file; `volume_fill.svg` and `sidebar.svg` were rewritten into equivalent shapes that Direct2D's SVG renderer supports.
The designer's video about it: https://youtu.be/ERFeOgqk324.
This project is not affiliated with Juxtopposed or with VideoLAN.

The Inter font in [assets/fonts](assets/fonts) is by The Inter Project Authors and licensed under the SIL Open Font License 1.1 ([OFL.txt](assets/fonts/OFL.txt)).
