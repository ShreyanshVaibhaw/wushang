<p align="center"><img src="assets/wushang.svg" width="128" alt="wushang logo"></p>

# wushang

A small macOS video player in Rust, built with the same pipeline as VLC but without its plugin system.
FFmpeg demuxes and decodes, VideoToolbox decodes on the GPU, Metal presents, and Core Audio plays the sound.
The interface follows Juxtopposed's VLC redesign (see Credits).
This is the macOS port of the Windows player; "Differences from the Windows version" near the end lists what behaves differently here.

## Download

Get `wushang-<version>-macos-arm64.dmg` from the [latest release](https://github.com/ShreyanshVaibhaw/wushang/releases/latest), open it, and drag wushang into Applications.
It needs a Mac with Apple Silicon and macOS 13 or later, and nothing else installed: FFmpeg and libass come inside the app.
To remove it, drag wushang from Applications to the Trash; the library and settings stay in `~/Library/Application Support/wushang` until you delete that folder.

The same app comes as `wushang-<version>-macos-arm64.zip`; unzip it and move `wushang.app` wherever you keep apps.

The app is signed ad hoc, not notarized by Apple, so macOS won't open it the first time with a double-click.
Right-click it in Finder, choose Open, then Open again in the dialog.
Where macOS offers no Open there (macOS 15 and later), double-click it once, then click Open Anyway in System Settings > Privacy & Security.
After that it opens like any other app.

Once it has run, Finder lists it in "Open With" for video and audio files.
To make it the default player for a type, select a file of that type in Finder, choose File > Get Info, pick wushang under "Open with", and click Change All.
It never takes a type over by itself.
A copy you move should be opened once from its new place, so Finder knows where it is.

### Updates

When wushang starts, it asks GitHub for the latest release, and when that one is newer it offers it in a window once nothing is playing.
Download saves the new `.dmg` (or the `.zip` when a release has no disk image) to `~/Downloads` and shows it selected in Finder.
Quit wushang, open the disk image, and drag wushang into Applications over the old copy.
The library, settings and history stay as they were.
wushang never replaces its own files: an app that rewrites itself is what malware does, and on macOS it would also break the app's signature.
Later closes the window, and wushang asks again next time it starts.
Settings > General > "Check for updates when wushang starts" turns the check off.

## Run from source

```bash
brew install rust ffmpeg libass pkgconf
cargo run --release -- path/to/video.mp4
```

(See Build requirements for what that needs and for building the app itself.)
A bare exe from `cargo run` has no Info.plist, so it isn't offered in Finder's "Open With"; the app from [scripts/bundle-macos.sh](scripts/bundle-macos.sh) is.

To make a release, raise `version` in [Cargo.toml](Cargo.toml), build the app as described under "Building the app", and attach `wushang-<version>-macos-arm64.dmg` and `.zip` to a GitHub release tagged `v<version>`.
The updater only takes files named that way from this repository's releases.

Without a file, wushang opens on the library.
The first time it does (no `settings.txt` yet), the design's welcome shows over it in three steps.
The first shows the media folder the library starts from, and Browse adds another to Videos.
The second picks the theme and accent colour, which apply as you click, over a small picture of the player.
The third lists the basic keyboard shortcuts; Finish, or Esc at any step, closes it.
`--sw` forces software decoding, which is useful for comparing the two paths.
`--play-and-exit` plays the file from the start and quits at its end, like VLC's option of the same name; the measurement scripts use it.

### Library

The library page follows the redesign's library frames.
The sidebar has Home, Videos, Music, Network, Playlists and Recent, plus "Add item", which adds a folder through the system folder picker.
On first run Videos and Music start with your `~/Movies` and `~/Music` folders.
Folders open in place, with back and forward arrows and a breadcrumb that shortens to "..." when the window is narrow.
The header shows the item count, the sort menu (unwatched or watched first, newest or oldest, title, artist or album; the orange dot means it isn't title A-Z), grid or list view, and a search box that filters the page as you type.
Thumbnails come from a frame at 10% of each video, and audio files show their embedded cover.
A bar under a thumbnail shows how much of it you watched, and finished files are dimmed.
Music also has Songs, Albums, Artists and Genres, built from the files' tags.
The tags are kept in `tags.txt` next to the library state, so these fill in at once on the next start; only new or changed files are read again.
Home collects Continue Watching, Listen Again, and your most visited folders, playlists and artists.
Dot files and files with Finder's hidden flag stay out of the lists unless Settings > Library shows hidden files.

Clicking a file plays it, resuming where you left it.
At the end it moves on to the next file of the page it came from, as VLC's playlist does.
Esc or the folder name in the player's title bar goes back to the library, and the video keeps playing in the mini player at the bottom right; click it to return.

In the player, the sidebar button opens the design's overlay panel over the video: a rail of section icons and a compact list of the same library, with history, breadcrumb and sort.
Clicking a file there plays it with the panel still open; a click on the video or Esc closes it, and the arrow button at its top right opens the full library page.
Dropping a file on the window or on wushang's Dock icon plays it, or adds it to the playlist on screen.
"Create playlist" and the header's + open the New Playlist dialog from the design: a name, an optional thumbnail picture, and files picked or dropped on the window.
On a playlist's page the header's + adds files to it.
Playlists are standard M3U files.

Right-click (or Control-click) a file, folder or playlist, on the library page or in the player's sidebar panel, for the design's context menus:
- files: play, mark as watched, favourite, add to the queue or a playlist, show in Finder, go to its folder, add a subtitle file, properties, and remove from the playlist when on a playlist's page;
- folders: add files or a folder, rescan, info, and the subtitle and audio languages its files start with;
- playlists: play, shuffle, favourite, export as M3U, add to the queue, rename (Properties & Metadata), delete, and the same track languages.

A folder's or playlist's track languages go over Settings' for its files: "Japanese audio, English subtitles" for an anime folder, say.
A folder's choice covers the folders inside it, the nearest choice wins, and a playlist's applies to files no chosen folder holds.
Favourites appear first on Home.

Library state (folders, recent files and positions, visit and play counts) lives in `~/Library/Application Support/wushang`, or in the folder named by the `WUSHANG_HOME` environment variable, which never seeds your own folders and suits tests.

### Player

The player has the redesign's own title bar and control bar; macOS's own title bar and window buttons are hidden.
Click play/pause, previous (restart, or the previous file within the first 3 seconds), next, the seek bar, the volume wedge (up to 200%, like VLC) and the speaker (mute).
Hovering over the seek bar shows a preview above it, as on YouTube: the frame at that point (its nearest keyframe, decoded in software on a thread of its own) and the time.
Dragging the seek bar pauses and shows the video under the pointer as it moves, then plays on from the drop if it was playing.
While the pointer moves it jumps to keyframes, the next jump waiting for the last one's frame, so the picture keeps up; where the pointer rests, an exact seek shows the frame under it.
The title bar has minimize, maximize (macOS's zoom), fullscreen and close, and dragging it moves the window.
A double-click on it zooms, minimizes or does nothing, as System Settings > Desktop & Dock says for every window.
While playing, the bars hide after 3 seconds without mouse movement and the video fills the window inside a 10 px rounded frame; moving the mouse brings them back.
The "…" button opens the main menu from the design: open files, open a folder, recent files, settings profiles, keyboard shortcuts, Customize Player, Settings and About.
Right-clicking the video opens the player menu: play, stop, previous, next, hide the controls, the mini player, and submenus for speed and sleep timer, video tracks and transforms, audio tracks, stereo mode and output device, subtitles, the settings tabs, the queue, and opening media.
Items that need something first, like Convert with nothing open, are shown dimmed.

The menu bar has what a Mac app is expected to have, each item doing what the design's menus do:
- wushang: About wushang, Settings… (⌘,), Hide wushang (⌘H), Hide Others (⌥⌘H), Show All, Quit wushang (⌘Q);
- File: Open File… (⌘O), Open Folder… (⌘F), Open Network Stream… (⌘N);
- Playback: Play/Pause;
- Window: Minimize, Mini Player (⌘M), and the window itself.

The playlist button opens the queue panel from the design, in the settings panel's place, with three tabs:

- Queue: the files previous and next step through, with their lengths and the total.
  The playing file is orange with its progress underneath, and files watched to the end are dimmed.
  Click a file to play it, drag it to reorder, and use sort (by name), repeat (off, all, one), shuffle, minus (removes the file last clicked) and + (adds files).
  Repeat One replays the file when it ends, while next and previous still move on; shuffle plays every file once before Repeat All wraps.
  A file's "…" or a right-click on it opens the design's file menu, where Play plays that row, Play Next queues it after the playing file (shuffled or not), and Remove from Queue takes it out.
- Bookmarks: positions in the playing file; + adds one where it is, a click seeks there.
  They live in `bookmarks.txt` next to the library state.
- Chapters: the file's chapters with their titles; a click seeks there.

The audio and subtitle buttons open the track popup from the design: video, audio and subtitle tracks (with names like "English" from the file's language tags), the output device, and external subtitle files, with + to load one by hand and the pencil to open the chosen one in your text editor (TextEdit unless you picked another).
"Add Audio File..." in the player menu's Audio submenu plays another file's sound with the video, like a dub or commentary kept beside it; it joins the audio track list under its file name and follows every seek.
A subtitle file with the same name as the video, next to it or in a "Subs" folder, loads on its own, as in VLC.
ASS and SSA subtitles, in the file or beside it, are drawn by libass as VLC and mpv draw them: their positions, fonts, colours, borders, shadows, rotation, karaoke and animation, with the fonts a Matroska file carries for them.
After a seek, a resume or a switch of subtitle track, a second reader goes over the 10 seconds before the new position, so an embedded line that began there shows at once instead of the screen staying empty until the next one.
On anamorphic video (DVDs and broadcasts store 16:9 in 720x576) libass draws at the shown size, and letters keep the shape they had where the script was laid out, as VSFilter and mpv show them.
Without `libass.9.dylib` (inside the app, or Homebrew's) they fall back to plain text like every other format.
Old subtitle files in a Windows code page instead of UTF-8 are read in the right one: a language tag such as `film.ru.srt` names it, and otherwise wushang tries the Western, Central European, Cyrillic, Greek, Turkish, Hebrew, Arabic, Baltic, Japanese, Chinese and Korean code pages and keeps the one whose text reads like real words.
The gear opens the settings panel, with four tabs:

- Playback: marks on the seek bar where the subtitles have lines, playback statistics over the video, reverse playback, a sleep timer, an audio visualizer, and speed from 0.25x to 2x without changing pitch.
- Video: flip, rotation, zoom, resolution, aspect ratio, crop, render, and brightness, contrast, hue and saturation (under "Expand Settings"), all done on the GPU in the Metal shader that shows the video.
  Zoom starts at Fill, which covers the window and cuts off what overflows; "1:1 Original" shows the whole picture with bars, and Reset goes back to Fill.
- Audio: track, stereo mode (stereo, mono, left, right, reverse) with a Test button, audio delay, and VLC's 10-band equalizer with its 18 presets and the "2 Pass" option.
- Subtitles: track, subtitle delay, speed and duration factor, text size, whether ASS and SSA keep their own styling, font, and text and outline colours.

Reverse plays backwards by stepping back with exact seeks, silent, as fast as the frames decode; Space or play stops it.
The visualizer draws a spectrum or an oscilloscope in place of the cover while an audio file plays.
Resolution picks between a stream's video tracks by picture size, "Best Available" being the largest, and Render adds streaming to other devices while the video still plays here.
The stereo Test plays a low tone on the left, then a high one on the right, through the chosen stereo mode.
The subtitle timeline knows every line of a subtitle file, but only the lines an embedded track has read so far.
The footer's preset menu switches settings profiles, and "Open Advanced Settings" opens the Settings window.
HDR "Auto" shows HDR10 and HLG video brighter than SDR white on a screen that can show EDR (extended dynamic range), and "Off" tone maps it to SDR there too.
Switching tracks and seeking land on the exact moment: the decoders read from the keyframe before it and drop what comes earlier.

| Key | Action |
|---|---|
| Space | Pause and resume |
| ← / → | Skip back or forward: 10 s, ⌥ 3 s, ⇧ 1 min, ⌘ 5 min (all set in Settings) |
| ↑ / ↓, mouse wheel or trackpad | Volume, 10% a press (scrolling moves the library) |
| M | Mute |
| F or double-click the video | Fullscreen (macOS's own, in a Space of its own) |
| Esc | Close a menu, panel or dialog, leave fullscreen, or go to the library |
| Delete (⌫) | Back, in the library |
| S / P / N | Stop, previous file, next file |
| ⌘O / ⌘F | Open files / open a folder |
| ⌘P or ⌘, | Settings |
| ⌘\ | Hide the controls until the mouse moves |
| ⌘M | Mini player, and back |
| ⌘I | Properties of the file playing, or of the file under the pointer |
| F1 / ⇧F1 | Keyboard shortcuts / About (hold fn on keyboards whose top row is media keys) |
| Q, ⌘Q | Quit |

Control works in place of ⌘ for every shortcut, as Ctrl did on Windows, except where macOS keeps the key for itself (Control with the arrows switches Spaces unless that is turned off in System Settings > Keyboard > Keyboard Shortcuts).
In a text field ⌥← and ⌥→ move by word, ⌘← and ⌘→ go to the start and end, ⌥⌫ deletes a word and ⌘⌫ deletes back to the start, as in other Mac apps.
Letter shortcuts follow the key's letter on your keyboard layout, so ⌘M is the key marked M on an AZERTY keyboard too.

Started hidden (`open -j -a wushang`), the window stays hidden and doesn't take focus.

### Snapshots

⇧S, "Take Snapshot" in the player menu's Video submenu, or the snapshot button from Customize Player saves the frame on screen as a PNG in `~/Pictures/wushang`; Settings' Recording page can make it a JPEG, or save it on the desktop or next to the file.
R, "Record" in the player menu, or the record button from Customize Player records what plays into a Matroska file in `~/Movies/wushang`, copied packet for packet without re-encoding, until R is pressed again or the media ends.
The Recording page can switch it to MPEG-TS, which keeps what was written if wushang stops mid-recording but holds fewer codecs, and save it on the desktop or next to the file.
The file holds what was on screen: it starts at the keyframe before the picture (copied video can only start on one), packets go in as playback reaches them rather than as the demuxer reads ahead, and a seek while recording carries on in the same file instead of jumping in it.
Network streams record too; the recording button lights while it runs.
It is the full-resolution frame with flips, rotation, crop, aspect ratio and colour adjustments as shown, but not zoom or the interface.
The file is named after the video and the moment in it, such as `Holiday 1m05s.png`, and a line over the video says where it went.

### Customize Player

"Customize Player" in the "…" menu or the player's right-click menu opens the design's palette over the player, and every item and group in the bars gets a dashed box.
Drag an item from the palette into the title bar group or the control bar's left, centre or right group, between items or across groups, and drag one back onto the palette to take it out.
Dropping on the seek bar starts the centre group, and the control bar then takes two rows with the seek bar across the top, as in the design's customize frames.
Save keeps the bars in `toolbar.txt` next to the library state; Cancel or Esc drops the changes.

Besides the bars' own controls, the palette has spacers and dividers, snapshot, playback speed, skip 10 or 15 seconds either way, stop, repeat, shuffle, the mini player, zoom, quit, Settings, the library, the file name, properties, replay, chapters, favourite, add a bookmark, bookmarks and playlists.
The thumbnail item shows the playing file's picture, Artist Name and Playlist Name the track's artist and the playlist the queue came from (each opens its page when clicked), and Right Sidebar opens and closes the queue.
The HDR item switches HDR output between Auto and Off, and lights while it is on Auto.

### Mini player

The mini player is the design's "Miniplayer" card: the video, its progress bar, the file name and folder, and a queue button.
It shows at the library's bottom right while something plays, and ⌘M (or "Mini Player" in the player's right-click menu or the Window menu) turns the whole window into it.
From fullscreen, the window first leaves fullscreen, then shrinks.
The mini player window stays above other windows and opens at the bottom right of the screen, clear of the Dock and the menu bar, or where you last left it.
Drag it by its body to move it.

With the pointer on the card, the video dims and shows the design's controls: previous, play/pause and next in the middle, close at the top left and expand at the top right.
Close stops playback; expand goes back to the full player, and in the window it restores the window's old size and place.
The progress bar seeks with a click or a drag.
The queue button opens the queue above the video, in the design's style, with sort, remove and add; the window grows upward to make room.
Esc or ⌘M also leaves the mini player window.

### Tabs

Up to 8 files can play at once, each in a tab of its own.
⌘T (File > New Tab, or the + by the window buttons) picks files to play in a new tab, and "Play in New Tab" in a library entry's right-click menu plays that file in one, with its folder as the tab's queue.
Opening a file any other way plays it in the tab on screen, as before.
With two or more tabs open, they take the title's place in the player's title bar.
Click a tab to show it, or use ⇧⌘] and ⇧⌘[ (or ⌃Tab and ⌃⇧Tab) for the next and previous tab, ⌘1 to ⌘8 for that tab and ⌘9 for the last.

Only the tab on screen is heard.
The others play on silently, marked with a small play sign, so switching to one shows its picture at once and brings its sound back.
Each tab keeps its own queue, position, tracks, subtitles and picture settings; the volume, mute, output device and sound effects are the window's.
⌘W (File > Close Tab, or a tab's ×) closes a tab and keeps its place, as Stop does, and closing the last tab stops playback.
When the tabs get narrow they drop their × and then show only their number; ⌘W still closes them.

### Network

⌘N, "Open Network Stream" in the "..." menu or the File menu, or "Add Network Stream" on the Network page asks for an address and plays it: http, https, HLS (`.m3u8`), RTSP, UDP or RTP.
A bare host like `example.com/live.m3u8` gets `http://` in front, as in VLC, and `wushang <address>` plays one from the command line.
The address stays on the Network page; right-clicking it offers Remove.
A stream connects in the background while the player says "Connecting to ...", and if it can't play, the corner says so before the library comes back.
A live stream, which has no end, shows "Live" in place of its length.
Podcasts takes an RSS or Atom feed and lists its episodes, newest as the feed orders them, under the feed's own title; right-clicking a podcast removes it.
A feed fetched more than 10 minutes ago fetches again when its page shows, keeping the episodes on screen meanwhile, and keeping them if the refresh fails.
Radio lists the 100 most voted stations of the community [Radio Browser](https://www.radio-browser.info) directory, with their genre and country.
Episodes and stations play with the name the page shows, and next and previous move through the page.
Feeds and the directory load over HTTP with the [ureq](https://crates.io/crates/ureq) crate on a background thread, so the window never waits on them.

### Capture devices

"Open Capture Device" in the "..." menu lists the cameras, capture cards and microphones AVFoundation sees, and each screen.
Picking one plays it live, and Record saves it like any other media.
The first time wushang opens a camera, a microphone or a screen, macOS asks whether to allow it; until you do, opening the device fails.
Camera and microphone access can be changed later in System Settings > Privacy & Security, where screen recording is listed too.
A camera opens at its biggest picture up to 1080p that runs at 30 frames a second, and a screen at 30 frames a second with the pointer in it.
The list is gathered in the background and again each time the menu opens, so a camera plugged in later shows up.

### Discs

"Open Disc" in the "..." menu plays a DVD or Blu-ray: a disc in a drive, a disc's files copied to a folder (⌘D), or a disc image (.iso, under "Blu-ray Image...").
The menu lists every mounted volume that holds a disc's file system (UDF, ISO 9660, audio CD) or a VIDEO_TS or BDMV folder at its top, such as a disc copied to a USB stick, by the name Finder shows.
A disc folder or image also opens from the command line or when dropped on the window.
An image is mounted read-only with `hdiutil`, out of Finder's sight, and detached when wushang quits; it may hold a Blu-ray or a DVD.
For a Blu-ray, wushang reads the playlists in `BDMV/PLAYLIST` itself and plays the longest one's clips back to back, with its length and chapter marks; that is the title libbluray would pick, and the FFmpeg here has no libbluray.
DVD needs no library either: wushang reads the disc's IFO files, picks the title set whose main program chain runs longest, and plays its VOB files back to back, with the length and chapters the IFO gives.
libdvdnav and libdvdread would add menus and every title, but would make the FFmpeg build GPL.
Neither plays encrypted discs (CSS on DVDs, AACS on Blu-rays), so commercial discs don't play; discs you made yourself do.

### Streaming to other devices

"Stream to Other Devices" in the player menu, or "Stream" in a file's right-click menu, serves what plays over HTTP as MPEG-TS, copied without re-encoding, to any number of players on the network: VLC, mpv, Kodi or a TV opens the address the corner shows, like `http://192.168.1.101:8080/`.
It listens on port 8080, or the next free one; the Recording page picks another starting port.
While it runs, the player menu's "Streaming" submenu shows that address, how many players are connected, and Stop Streaming; the stream also stops with its media.
Like a recording it follows playback, so pausing or seeking here pauses or moves the stream.
H.264 and HEVC get their parameter sets repeated before every keyframe, so a player that connects midway starts playing at the next one.
MPEG-TS can't carry VP9, AV1 or the lossless audio formats; such media says so, and converting it to MP4 first works.
macOS may ask once whether wushang may find and connect to devices on your local network, and, with the macOS firewall on, whether it may accept incoming connections, since other devices connect to it.

### Converting

"Convert" in a file's right-click menu, or in the "..." menu for the open file, re-encodes it into an MP4 (H.264 and AAC, saved in `~/Movies/wushang`) or an MP3 (the sound alone, saved in `~/Music/wushang`); ⌘R makes an MP4 of the open file.
H.264 comes from macOS's VideoToolbox encoder (`h264_videotoolbox`) and AAC from AudioToolbox (`aac_at`), so the FFmpeg build needs no encoder library for them and stays LGPL.
MP3 comes from LAME (`libmp3lame`, LGPL), which the release's FFmpeg and Homebrew's both have; an FFmpeg without it saves the sound as AAC in an M4A instead.
Where macOS's encoders are missing, FFmpeg's own `aac` and, in a build that has it, `libx264` stand in.
The conversion runs in the background with its progress in the corner, on the player or the library, and "Stop Converting" in the "..." menu ends it and deletes the unfinished file.
Video gets about 0.08 bits per pixel per frame (5 Mb/s for 1080p at 30 frames a second), sound 192 kb/s.

### Settings

⌘P, ⌘, or Settings in the "..." menu opens the Settings window from the design's "Settings" frames: categories on the left, their options on the right, and a search box that finds an option in any category.
Every option it lists does something.
Extensions holds wushang's optional add-on, libass: whether ASS and SSA keep their own styling, and whether it uses the fonts a Matroska file carries.
VLC's Lua extensions don't run in wushang.
Changes apply at once and are kept in `settings.txt` next to the library state.

- General: dark, light or system theme, and the accent colour.
  The design has one light frame, and wushang turns every screen light from it, so menus, panels and dialogs follow the same greys.
  System follows macOS's Light or Dark appearance (System Settings > Appearance) and switches when it changes.
  The accent replaces the design's orange everywhere except the logo and the volume meter.
  It starts as the logo's red-orange; the design's amber is the next choice along.
  Its nine colours are the onboarding frames' bright ones; Settings dims all but the one picked, as the design does.
- Interface: keep the window above others, remember its size and position, and when and how fast the controls hide.
- Privacy: whether recently played files are saved, and a button that clears the history.
- Controls: the four skip lengths, what a click, a double-click and the wheel do on the video, and the keyboard shortcuts.
- Playback: resuming where you left off, starting paused, the default speed, the preferred audio language, repeat, shuffle, and quitting at the end of the queue.
- Video: start in fullscreen, keep the screen awake while a video plays, and the default aspect ratio.
- Audio: output device, startup volume (or the last one), the Up/Down volume step, the equalizer preset, two-pass and stereo mode.
  "Custom" is the last equalizer tuned by hand in the panel, kept while a preset plays.
- Subtitles: on or off, the preferred language, loading subtitle files on their own, and their size, weight, colour, opacity, outline and shadow.
  The fonts to pick from are Inter, which comes with wushang, and Helvetica Neue, Arial, Verdana, Tahoma, Georgia, Times New Roman and Menlo, which come with macOS.
- Library: show hidden files and folders.
- Network: reconnect when a stream drops, and how long a stalled connection may wait (15 seconds by default).
  FFmpeg reconnects http and HLS itself; an RTSP, RTP, UDP, TCP, SRT or RTMP stream that drops is opened again, each second until it answers or the timeout passes, with "Reconnecting..." over the video.
- Performance: turn off GPU decoding.

The equalizer, stereo mode, subtitle size, repeat and shuffle chosen in the panels are kept the same way.
Speed, aspect ratio, tracks and decoding are per-file defaults, so a change there applies from the next file.

### Profiles

A profile is a whole set of settings under a name.
The "…" menu's Profile row names the one in force, and its submenu, from the design's "Profiles" frame, lists Movies, Shows, Music and Work, then any you added, with "New" at the bottom.
Picking a profile applies its settings at once, theme and accent included, and wushang starts in it next time.
A profile starts as a copy of the settings in force when it is first picked or made, and changes from then on stay with it.
The window's place and the volume are left alone, since they belong to the machine rather than the profile.
"New" asks for a name, then adds the profile and switches to it.
Each profile keeps its settings in `profiles/<name>/settings.txt`, and `profile.txt` names the one in force.

## Build requirements

- A Mac with Apple Silicon.
- Rust 1.88 or newer, from Homebrew (`brew install rust`) or rustup.
- Apple's Command Line Tools (`xcode-select --install`); full Xcode isn't needed.
  The Metal shaders are compiled from source when wushang starts, so no `metal` compiler is needed either.
- For development, Homebrew's `ffmpeg` and `libass`, and `pkgconf`.
  `ffmpeg-sys-next` finds FFmpeg through pkg-config, and wushang loads libass from `/opt/homebrew/lib` at run time.
- libclang, which `ffmpeg-sys-next` needs to generate bindings at build time.
  [.cargo/config.toml](.cargo/config.toml) points `LIBCLANG_PATH` at the Command Line Tools' copy in `/Library/Developer/CommandLineTools/usr/lib`.

Homebrew's FFmpeg is GPL (it is configured with `--enable-gpl`, x264 and x265) and its libraries are built for the Mac's own macOS version, so it is only for building and running here.
The app that is passed on carries the slim LGPL builds below, made for macOS 13 and later.
[build.rs](build.rs) links the exe with an rpath to `@executable_path/../Frameworks`, where the app keeps them, and room in its load commands for the bundle script to repoint it.

The two build scripts and the bundle script use a `tools` folder beside the repository by default, as the commands below do.
Both build scripts need Homebrew's `meson`, `ninja` and `pkgconf`, download their tarballs into `SRC` when they aren't there, check them against SHA-256 sums written in the script, and unpack them afresh on each run.

### Building the slim FFmpeg

[scripts/build-ffmpeg-macos.sh](scripts/build-ffmpeg-macos.sh) builds FFmpeg 9.0.2 as dylibs for macOS 13 and later (`MACOSX_DEPLOYMENT_TARGET=13.0`), with dav1d 1.5.4 and LAME 4.0 linked into them statically.

```bash
SRC=../tools/src/macos PREFIX=../tools/ffmpeg-9.0.2-wushang scripts/build-ffmpeg-macos.sh
```

It builds the file protocol and the network ones a player opens (http, https, tls, tcp, udp, rtp, with TLS from macOS's Secure Transport, so there is no OpenSSL), `concat` for a disc's files, 37 demuxers (HLS and RTSP among them), 20 parsers, 59 decoders (raw video and PCM among them, for capture devices, plus ProRes, Monkey's Audio, AMR and PNG for cover art) and the VideoToolbox hwaccels for H.264, HEVC, AV1, VP9, MPEG-2, MPEG-1 and ProRes, plus libswresample and libswscale.
For recording and converting it adds 10 muxers (Matroska, MP4 and MPEG-TS among them), every bitstream filter, and the `h264_videotoolbox`, `hevc_videotoolbox`, `aac_at`, `aac` and `libmp3lame` encoders.
libavdevice holds the AVFoundation input, for cameras, microphones and screens.
One of the decoders is libdav1d, so AV1 plays in software on Macs whose GPU doesn't decode it.
zlib, which macOS has, lets the Matroska reader open tracks stored zlib-compressed, which mkvmerge does to subtitles by default.
There is no libbluray (wushang reads Blu-ray playlists itself), no libavfilter, and no command-line programs.
The script stops if configure doesn't report "LGPL version 2.1 or later" or leaves out one of the pieces wushang needs, and checks that each dylib is built for macOS 13, named by `@rpath`, and needs nothing but macOS and the other FFmpeg dylibs.

### Building libass

[scripts/build-libass-macos.sh](scripts/build-libass-macos.sh) builds libass 0.17.5 with FreeType 2.15.0, FriBidi 1.0.18 and HarfBuzz 15.0.0 linked into one `libass.9.dylib`, for macOS 13 and later, which exports only libass's own functions and needs only system libraries.
Fonts come from macOS through Core Text, so there is no fontconfig.

```bash
SRC=../tools/src/macos PREFIX=../tools/libass-0.17.5 scripts/build-libass-macos.sh
```

wushang loads it at run time ([src/ass.rs](src/ass.rs)) from the app's Frameworks folder, else from Homebrew's `/opt/homebrew/lib` or `/usr/local/lib`, so a build without it still runs.

### Building the app

[scripts/bundle-macos.sh](scripts/bundle-macos.sh) builds the release, packs it as `wushang.app`, and makes `wushang-<version>-macos-arm64.dmg` (the app beside a link to Applications, to drag across) and `wushang-<version>-macos-arm64.zip` from it.
The disk image and zip go in the `bundle` folder of the build folder, the `target_directory` that `cargo metadata --format-version 1 --no-deps` names, and the app in `bundle/app.noindex`.
The `.noindex` folder keeps that copy out of Spotlight, the script takes it back out of Launch Services, and wushang doesn't register a copy there, or one run from its mounted disk image, so only the installed app shows in Spotlight and "Open With".
macOS itself still records any app that is launched, so running that copy puts it back in "Open With" until the next bundle run; try builds from `/Applications` instead.

```bash
scripts/bundle-macos.sh
```

It sets `FFMPEG_DIR` and `MACOSX_DEPLOYMENT_TARGET=13.0` for its own release build, so `ffmpeg-sys-next` links the slim FFmpeg instead of Homebrew's.
The script takes FFmpeg from `FFMPEG_DIR` and libass from `LIBASS_DIR` (by default `../tools/ffmpeg-9.0.2-wushang` and `../tools/libass-0.17.5`), and stops at an exe that needs a library from anywhere else, such as Homebrew's GPL FFmpeg.
It builds with your home folder cut out of the source paths the exe embeds, so the app doesn't carry your user name.
It copies every library the exe links, and libass, into `Contents/Frameworks` and points the exe and each library at the copies, fills in the version from Cargo.toml in [assets/macos/Info.plist](assets/macos/Info.plist), draws the icon from `assets/wushang.svg` when the SVG has changed, and copies the licence files into `Contents/Resources`.
It stops when a library was built for a newer macOS than the Info.plist's minimum, or when the exe or a library has a strong import that macOS doesn't have, since either would keep the app from loading there.
Then it signs the libraries and then the app ad hoc, and checks the signature.

- `scripts/bundle-macos.sh --no-build` bundles the release exe already built.
- `WUSHANG_EXE=/path/to/exe scripts/bundle-macos.sh --no-build` bundles another exe.

To use your own build, copy `wushang.app` from the bundle folder into Applications.

## How it works

```text
demux thread     libavformat reads packets and routes them to per-stream queues; it also seeks
video decoder    libavcodec + VideoToolbox decodes into CVPixelBuffers (software fallback)
audio decoder    decodes, resamples to f32 at the device's rate and channels, fills a 0.3 s ring
audio callback   Core Audio (through cpal) plays the ring and sets the master clock
                 (media time minus the time until the samples reach the speaker)
main thread      AppKit events and the menu bar, and the render loop: show a frame when its time
                 comes, skip late ones, paint the interface with Core Graphics, present with Metal
thumbnailers     two threads decode one still per visible file, newest request first
prober           one thread reads music tags for Songs, Albums, Artists and Genres
```

The window and the Metal device live for the whole run, and each tab has an audio device of its own for as long as it is open.
Opening a file starts a session ([src/session.rs](src/session.rs)): its demux and decoder threads, which stop when the next file opens in that tab or the tab closes.
A tab's audio device opens once, on a thread of its own, because opening one can take a while (a Bluetooth headset wakes up first); switching files only clears the ring.
macOS mixes the tabs' outputs, and the tabs out of sight send silence, so their clocks keep running and the render loop keeps taking their frames as they come due.
[src/library.rs](src/library.rs) lists folders, keeps recent files and positions, and reads and writes the M3U playlists; it doesn't touch FFmpeg.
[src/thumbs.rs](src/thumbs.rs) makes thumbnails and reads tags with FFmpeg in software, so it never competes with playback for the GPU decoder.
The library page ([src/ui/browse.rs](src/ui/browse.rs)) is painted inside a layer masked to everything but the mini player's video, so the grid can sit under the card while the live video shows through.

| VLC module | Here | File |
|---|---|---|
| access, demux, packetizer | libavformat | [src/player.rs](src/player.rs) |
| decoder, hw decoder (videotoolbox) | libavcodec with `hw_device_ctx` on VideoToolbox | [src/player.rs](src/player.rs) |
| vout display | a Metal shader drawing into a `CAMetalLayer`, with VideoToolbox frames read through `CVMetalTextureCache` | [src/render.rs](src/render.rs), [src/shaders/video.metal](src/shaders/video.metal) |
| audio output (auhal) | cpal on Core Audio | [src/audio.rs](src/audio.rs) |
| audio filters (scaletempo, equalizer, mono/channel remap) | WSOLA speed, VLC's equalizer, stereo modes | [src/dsp.rs](src/dsp.rs) |
| subtitle decoders and renderer (subsdec, freetype) | libavcodec subtitle decoders, captions drawn with Core Text | [src/subs.rs](src/subs.rs), [src/captions.rs](src/captions.rs) |
| libass_plugin | libass, loaded at run time | [src/ass.rs](src/ass.rs) |
| video filters (transform, adjust, crop) | the same Metal shader | [src/render.rs](src/render.rs), [src/shaders/video.metal](src/shaders/video.metal) |
| clock, late-picture dropping | audio master clock, skip to the newest due frame | [src/main.rs](src/main.rs) |
| Qt interface | Core Graphics + Core Text interface through the d2d shim, in an AppKit window with its title bar hidden | [src/ui.rs](src/ui.rs), [src/ui/browse.rs](src/ui/browse.rs), [src/d2d](src/d2d), [src/platform](src/platform) |
| media library (medialibrary) | folders, recent files, M3U playlists, thumbnails, tags | [src/library.rs](src/library.rs), [src/thumbs.rs](src/thumbs.rs) |

The decoder, the video shader and the interface share one Metal device.
VideoToolbox decodes into `CVPixelBuffer`s on IOSurfaces, and `CVMetalTextureCache` hands each one to the shader as two textures, luma and chroma, without a copy.
Nothing is copied back to system memory.
One shader ([src/shaders/video.metal](src/shaders/video.metal)) converts YCbCr to RGB with the frame's matrix and range, applies the colour adjustments and, for BT.2020, the tone map, and scales into the drawable; crop, aspect ratio, zoom, rotation and mirroring are all in where the quad's corners read the frame.
The shaders are compiled from source with `newLibraryWithSource` when the renderer starts, and each output format gets its pipelines the first time it is drawn to.

The interface is painted by Core Graphics and Core Text through [src/d2d](src/d2d), a shim with Direct2D's and DirectWrite's type and method names, so the interface code in [src/ui](src/ui) is the Windows code with little more than its `use` lines changed.
It paints into a premultiplied BGRA bitmap whose memory is a Metal buffer, shared between the CPU and the GPU on Apple Silicon, and the same render pass draws that bitmap over the video.
Two such bitmaps take turns, so the CPU paints the next frame while the GPU may still read the last.
The icons are the design's SVGs, rasterised by resvg at each scale they are drawn at and cached, and the text uses Inter, embedded in the exe.
Layout numbers come from the design's 1920x1080 frame in points, so the interface scales with the screen's backing scale factor.
While the window is hidden behind others or minimized, a present returns at once without drawing.

A stream VideoToolbox doesn't take is decoded in software from the start: MJPEG webcams, MPEG-4 Part 2 and everything but H.264, HEVC, AV1, VP9, MPEG-2, MPEG-1 and ProRes.
One it turns down after all, such as 10-bit H.264, which VideoToolbox doesn't decode, or a 4:4:4 AV1, is reopened in software on every core (libdav1d for AV1) and decoded again from the last keyframe.
The decoder thread repacks software frames to NV12, or P010 for 10-bit video so it keeps its precision, with swscale, splitting each frame across the cores, so the render thread only copies them to the GPU.
VideoToolbox frames in other layouts (one-plane formats such as ProRes 4444) are copied back and repacked the same way.

BT.2020 video (HDR10, HLG and SDR) is tone mapped in the same shader; [src/tonemap.rs](src/tonemap.rs) holds its numbers and the same math on the CPU, which the tests hold the shader to.
The steps are: to linear light, a matrix to the output's primaries (BT.709 for SDR, Display P3 for EDR), a tone curve per channel, then sRGB or the EDR layer's linear values.
HLG gets its system gamma on luminance, so its colours keep their saturation, and puts reference white at 203 nits.
SDR BT.2020 is decoded with BT.1886's gamma 2.4 and encoded with it again after the matrix to BT.709, so it has the tones BT.709 video has and only its colours change.
On an SDR screen the curve leaves everything up to three quarters of HDR's reference white (203 nits) as it is and rolls the highlights above it off into the rest of SDR's range, up to the stream's MaxCLL (else its mastering display's peak, else 1000 nits; 1000 nits for HLG).

On a screen that can show EDR, HDR10 and HLG frames switch the layer from 10-bit sRGB to half-float extended linear Display P3 with `wantsExtendedDynamicRangeContent`, and it switches back as soon as no such frame is on screen, so SDR video and the library never ask the screen for headroom.
macOS reports how bright EDR can go as headroom, how many times SDR white the brightest pixel can be right now; it moves with the brightness setting, so [src/hdr.rs](src/hdr.rs) reads it on every pass.
SDR white is taken as 203 nits, HDR's reference white, so the screen's peak is 203 nits times the headroom; with one of the reference presets of Apple's XDR screens (such as "HDR Video (P3-ST 2084)"), SDR white is 100 nits and the peak is what the preset allows.
HDR10 and HLG video keep their nits up to three quarters of the screen's peak, and the same curve rolls brighter highlights off into the rest.
The interface stays at SDR white, so it looks as it does in other apps.
BT.709 and BT.601 video is unchanged by any of this.

The render loop runs in the user-interactive QoS class, the scheduler's highest for an app, as VLC runs its video output thread above normal priority.
While the window is dragged or resized, a menu is open or a file picker is up, AppKit runs a loop of its own until it is done; a timer in those run-loop modes then runs the render loop's passes, so the video keeps playing.
A pass that fails (a lost frame) is logged and skipped rather than closing the player.

## Measured on macOS

M5 MacBook Air, its own screen at 60 Hz.

| 4K60 clip | Decoding | Presents per second | Late frames |
|---|---|---|---|
| HEVC 10-bit PQ, shown with EDR | VideoToolbox | 59.4 | 2 of 416, both at the start while the Metal pipelines were created |
| H.264 | VideoToolbox | 59.8 | 1 |
| HEVC 10-bit | software | 60.0 | 2 |
| H.264 | software | 60.1 | 1 |

The first frame is on screen 273 ms after launch.
On test clips with a known answer, the colour conversion comes within 0.47 levels of 255 of the exact one.
100 nits of PQ comes out as 0.7294 on an SDR screen, where the Windows build gives 0.726.

## AV1 in software

libavcodec always prefers libdav1d for AV1, and libdav1d never uses the GPU.
wushang asks VideoToolbox first (`VTIsHardwareDecodeSupported`), which says yes on M3 and later chips.
If it does, FFmpeg's own AV1 decoder runs on VideoToolbox like every other codec; if not, libdav1d decodes on every CPU core.
On Windows, libdav1d's bursts across all cores made a render loop at normal priority wake about 40 ms late and skip frames, which is why the render loop asks for the user-interactive class here.

## Known limits

Some of these are marked with a `ponytail:` comment in the code, along with the upgrade path.

- DVDs play their main title only, without menus or the other titles; Blu-ray menus (BD-J) don't play either, and a Blu-ray's main title is taken to be its longest playlist.
- Without a language tag, traditional Chinese (Big5) subtitles read as simplified (GBK), and Turkish or Baltic ones as Western, since the same bytes are valid in both.
- Contrast and saturation go up to 2x, like VLC's adjust filter; brightness adds up to 100 of 255 levels and hue turns up to half a circle either way, the ranges the Windows build's video processor has.
- EDR has been measured only on the M5 MacBook Air's own screen; the XDR reference presets and external HDR screens are handled in the code but haven't been measured.
- HLG is graded for a 1000-nit display whatever the screen's peak.
- 4K60 10-bit H.264 decodes in software, since VideoToolbox doesn't decode 10-bit H.264.
- The audio callback takes a mutex.
- The stereo Test sound plays through macOS's default output at the system volume, not the chosen device or wushang's volume.
- The subtitle timeline shows nothing for ASS and SSA tracks libass draws, and an embedded track's lines only as the demuxer reads them.
- A recording or network stream of an RTSP or UDP stream ends where that stream drops and reconnects.
- A reconnected stream must keep its track order; one that comes back with different tracks plays them wrongly.
- A video stream that fails its first 60 packets counts as undecodable, and playback goes on with audio only.
- A camera or capture card whose name has a colon in it can't be opened, because FFmpeg's AVFoundation input reads the colon as the end of the name.
- Thumbnails stay in memory for the whole run, about 350 KB each.
- https and HLS go through Secure Transport, which stops at TLS 1.2, so a server that only speaks TLS 1.3 can't be opened by the release build.
- The app is Apple Silicon only, and signed ad hoc rather than with a Developer ID, so it isn't notarized.

## Differences from the Windows version

The macOS app has the same features and interface; these behave differently, mostly to match how Mac apps work.

- Shortcuts use ⌘ where Windows uses Ctrl, and ⌥ where it uses Alt; Control is still accepted for ⌘, and the menus, the Keyboard Shortcuts dialog and Settings show ⌘, ⌥ and ⇧.
- There is a menu bar (wushang, File, Playback, Window), with ⌘, for Settings and ⌘H to hide, as Mac apps have.
  ⌘M is the mini player, as Ctrl+M was, so Minimize in the Window menu has no shortcut.
- Fullscreen is macOS's own, in a Space of its own with an animation, instead of a borderless window over the taskbar; the menu bar and the Dock hide while it is on.
- macOS's title bar and window buttons are hidden under the design's own, and a double-click on the title bar does what System Settings says.
- The video keeps playing while a file or folder picker is open; on Windows it paused on screen.
- Folders are the Mac's: `~/Library/Application Support/wushang` for the library and settings, and `~/Movies`, `~/Music`, `~/Pictures`, `~/Desktop` and `~/Downloads` in place of Windows' Videos, Music, Pictures, Desktop and Downloads.
  The window's place is kept in points from the main screen's top-left corner, and one from a Windows `settings.txt` copied over is ignored when it is off every screen; a Windows `tags.txt` is read again because its timestamps differ.
- Updates come as a disk image in `~/Downloads` to drag over the old copy; there is no installer.
- "Open With" and default apps come from the document types in the app's Info.plist, which Launch Services reads, instead of registry entries.
- Capture goes through AVFoundation instead of DirectShow and GDI, and macOS asks before wushang uses a camera, microphone or screen.
- Blu-ray goes through wushang's own playlist reader instead of libbluray, images are mounted with `hdiutil`, and DVD images play too.
  The disc list shows only discs that are in a drive, where Windows listed every optical drive.
- Converting uses VideoToolbox, AudioToolbox and LAME instead of Media Foundation's encoders.
- HDR uses EDR, which wushang asks for only while HDR10 or HLG is on screen, instead of a screen switched to HDR mode in Windows' display settings.
- The subtitle fonts offer Helvetica Neue and Menlo where Windows offers Segoe UI and Consolas.
- Streaming to other devices may bring up macOS's local network prompt instead of the Windows firewall's.
- Starting hidden is `open -j`, which stands for Windows' "Run: Minimized" shortcuts.
- Tabs, for several files playing at once, are on macOS only so far; the Windows version plays one file at a time.

## Licence

wushang is under the MIT licence ([LICENSE](LICENSE)).
The libraries, crates, design and font it uses keep their own licences, listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) with full texts in [licenses](licenses).
[scripts/bundle-macos.sh](scripts/bundle-macos.sh) copies all three, and the font's licence, into `wushang.app/Contents/Resources`, so a copy of the app can be passed on as it is.

The FFmpeg and libass in the app are the builds from the scripts above: FFmpeg is LGPL, which allows a MIT player as long as FFmpeg stays dynamically linked.
Homebrew's FFmpeg is GPL, and an app bundled with it would be GPL, which is why the bundle script only takes the slim build.

## Credits

The interface is based on [VLC Redesign](https://www.figma.com/community/file/1691465999134370980/vlc-redesign) by Juxtopposed, shared on the Figma Community under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
The icons in [assets/icons](assets/icons) are exported from that file, except the logo; `volume_fill.svg` and `sidebar.svg` were rewritten into equivalent shapes that the Windows build's Direct2D SVG renderer supports.
The designer's video about it: https://youtu.be/ERFeOgqk324.
This project is not affiliated with Juxtopposed or with VideoLAN.
VLC is a trademark of VideoLAN.

The wushang logo is [assets/wushang.svg](assets/wushang.svg); [assets/icons/logo.svg](assets/icons/logo.svg) is its W alone, for the title bar, and [assets/macos/wushang.icns](assets/macos/wushang.icns) the app's icon, which [scripts/bundle-macos.sh](scripts/bundle-macos.sh) draws from the SVG.

The equalizer presets are VLC's, under LGPL 2.1 or later.

The Inter font in [assets/fonts](assets/fonts) is by The Inter Project Authors and licensed under the SIL Open Font License 1.1 ([OFL.txt](assets/fonts/OFL.txt)).
