# How VLC 3.0.23 plays a video

Reverse-engineered on 2026-10-25 from the copy installed at `C:\Program Files\VideoLAN\VLC`.
Static analysis used REA with the Ghidra 12.1.4 provider.
Runtime evidence came from playing two synthetic 1080p clips with `-vv` file logging.
No personal media was opened.

| File | Version | SHA-256 |
|---|---|---|
| `vlc.exe` | 3.0.23 | `bfa5740f028a8f310ca01c779bbc5f2f1b435c41539c54de592f715bb0c10389` |
| `libvlccore.dll` | 3.0.23 | `d3475b834dd3eb77910f37f71b0341d358bcbdda5b9f04cc4a3a8e2be1bc8e35` |

The official site currently offers 3.0.24, so this install is one patch release behind.

Each claim below is tagged.
**Proven** means I read it in the shipped binary or saw it in a runtime log.
**Inferred** means it fits the evidence and the official docs but I did not read the code that does it.
**Hidden** means I could not see it.

## The shape of the install

VLC is a thin launcher, two core libraries and 366 plugin DLLs.

- `vlc.exe` (1 MB) is only a launcher.
- `libvlc.dll` (192 KB) is the public C API, with 317 `libvlc_*` names, 54 of them `libvlc_media_player_*`.
- `libvlccore.dll` (2.8 MB) is the engine: plugin loader, input thread, decoder threads, video output, audio output, clock.
- `plugins\` holds 366 DLLs in 29 folders, about 140 MB in total.
  The biggest folders are `codec` (61 files, 46 MB), `access` (40 files, 21 MB), `gui` (Qt and skins, 19 MB) and `demux` (47 files, 11 MB).
- `plugins\plugins.dat` is a cache that records every module's name, capability, score and entry function, so VLC does not have to load all 366 DLLs at startup.
  Its header reads `cache vlc 3.0.23`.

## Step 1: vlc.exe starts the engine (proven)

`vlc.exe` imports exactly seven functions from `libvlc.dll` (evidence `ev_c9f137…`).
Decompiling `WinMain` (`ev_9f3534…`) shows this order:

1. Harden the process.
   It calls `SetProcessDEPPolicy`, `SetDllDirectoryA("")`, `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` and `SetProcessMitigationPolicy(ImageLoad, PreferSystem32Images)`.
2. Convert the wide-char command line to UTF-8 and insert `--media-library` as the first argument.
3. Install the Breakpad crash handler into `%APPDATA%\vlc\crashdump`, unless `--no-crashdump` is given.
   Crash reports go to `https://win.crashes.videolan.org/reports` after the user agrees.
4. Read the UI language from `HKCU\Software\VideoLAN\VLC\Lang`.
5. `libvlc_new(argc, argv)`.
   If it fails, show "VLC media player could not start ... no plugins were found".
6. `libvlc_set_app_id("org.VideoLAN.VLC", "3.0.23")` and `libvlc_set_user_agent("VLC media player")`.
7. `libvlc_add_intf("hotkeys,none")`, `libvlc_add_intf("globalhotkeys,none")`, then `libvlc_add_intf(NULL)`, which loads the default interface (Qt, score 151 in the cache).
8. `libvlc_playlist_play(-1)`, then `libvlc_wait()` blocks until the user quits, then `libvlc_release()`.

The injected `--media-library` flag has a visible cost.
In both runtime logs the very first demux VLC opens is `%APPDATA%\vlc\ml.xspf`, before the file I asked it to play.

## Step 2: every job is a plugin chosen by score (proven)

`vlc_module_load` in `libvlccore.dll` (`ev_5be472…`, source path `src/modules/modules.c`) does the selection:

1. Fetch the candidate array for a capability string such as `"demux"` from a lookup tree (`ev_a35282…`).
2. Walk the comma-separated request, for example `"hotkeys,none"`.
   `"none"` stops the search, `"any"` means every candidate, and a leading `$` reads the request from a config variable such as `$codec`.
3. Call each candidate's open function in array order.
   The first one that returns 0 wins and VLC logs `using %s module "%s"`.
   A return of -3 aborts the search.
   With `"any"`, modules with score 0 or lower are skipped.

The scores live in `plugins.dat`.
I parsed it with a heuristic reader that matches length-prefixed capability strings followed by a 32-bit score.
Highlights for the playback path:

| Capability | Top modules (score) |
|---|---|
| demux | mp4 (240), avi (212), asf (200), es (155), ts (145), mkv (lower, found by extension) |
| video decoder | avcodec `ffmpeghw` hardware path (10001), dav1d (10000), png/jpeg (1000), avcodec software (70) |
| hw decoder | d3d11va (110), dxva2 (100) |
| vout display | direct3d11 (300), direct3d9 (280), glwin32 (275), gl (270), directdraw (230), wingdi (110) |
| audio output | mmdevice/WASAPI (150), directsound (100), waveout (50) |
| interface | qt (151), skins2 (30), lua (25) |

The runtime logs confirm the outcome.
Each `"any"` lookup (video decoder, 20 candidates; vout display, 12; audio output, 6) picked the highest-scored module that could open.

## Step 3: the pipeline for one file (proven)

This is what happened when VLC played `test_h264.mp4` (1080p30 H.264 + AAC), with the HEVC/Opus MKV run in brackets where it differed.

| Stage | What VLC does | Module chosen at runtime | Code evidence |
|---|---|---|---|
| Access | Open the byte source | `filesystem`, then stream filters `cache_read`, `record` | log |
| Demux | Pick a container parser | `mp4` [`mkv`] | `demux_New`, `ev_ba6b1c…` |
| Packetizer | Split raw streams into frames, only if the demuxer did not | none needed for MP4/MKV | `CreateDecoder`, `ev_840e18…` |
| Decoder | One decoder object and one thread per elementary stream | video `avcodec` + hw `d3d11va`; audio `faad` [`opus`] | `ev_840e18…`, `ev_a758fd…` |
| Video output | Convert, filter, time and present pictures | `direct3d11`, converters `swscale`, `yuvp`, text `freetype` | `ev_99926c…` |
| Audio output | Mix, resample, play | `mmdevice` (WASAPI) | strings `ev_6b78fc…` |

Details worth knowing:

- **Demux choice.**
  When the request is `"any"`, `demux_New` takes the file extension, binary-searches a 33-entry extension table and asks for that demuxer first.
  The log line `looking for demux module matching "mp4"` comes from exactly that.
  For network streams it maps the MIME type through an 8-entry table instead.
- **Decoder choice.**
  `CreateDecoder` indexes a table by stream category: 1 gives `"video decoder"`, 2 gives `"audio decoder"`, 3 gives `"spu decoder"` (`ev_a758fd…`).
  It first loads a `"packetizer"` module when the stream is not already packetized, which matches the MP4 run loading none.
- **Hardware decoding.**
  avcodec listed `dxva2_vld`, `d3d11va_vld`, `d3d11` and `yuv420p`, tried `d3d11va_vld`, and the `d3d11va` module reused the Direct3D 11 device that the `direct3d11` display had already created.
  Decoded frames stay on the GPU.
- **Queues.**
  Each decoder gets a `block_FifoNew()` input queue, a mutex and four condition variables.
  The core exports the `vlc_fifo_*` and `picture_pool_*` families that these queues and frame pools use (`ev_ec6139…`).

## Step 4: threads and timing (proven unless noted)

- `input_Start` spawns the input thread with priority 1 (`ev_050c55…`).
- `decoder_New` spawns one thread per decoder.
  Audio decoders get priority 2 and video decoders priority 0 (`ev_840e18…`).
  On Windows those map to `ABOVE_NORMAL`, `HIGHEST` and `NORMAL` (inferred from VLC's thread headers).
- `vlc_clone` has 15 direct call sites in the core (`ev_759822…`).
  The process ran 29 threads while playing one file.
- The video output drops late pictures in `ThreadDisplayPreparePicture` (`ev_99926c…`, `src/video_output/video_output.c`).
  It compares `mdate()` with the picture's date.
  If the picture is later than half a frame period (16.7 ms at 30 fps, or 20 ms when the frame rate is unknown), VLC drops it, logs "picture is too late to be displayed" and atomically bumps a lost-picture counter.
- The audio output keeps sync by resampling.
  The core has the messages "playback too late (%lld): up-sampling", "playback too late (%lld): flushing buffers" and "resampling stopped (drift: %lld us)".
  I did not decompile the clock or the resampler, so how VLC computes the drift is inferred.

## What I infer but did not read

- The candidate array for each capability is sorted by score when the plugin bank loads.
  The official module-loading page says VLC tries modules "in decreasing score order", and all three runtime `"any"` lookups agree, but I did not find the sort call.
- Audio is the master clock and video is scheduled against it.
  The resampling strings and the official core guide point that way.
- The avcodec submodule that actually opened was `ffmpeghw` (score 10001).
  The log only says `avcodec`.
- Struct field meanings (the "already packetized" flag, the frame-rate fields) come from behaviour plus VLC's public source layout, since no PDB symbols loaded.
- The other 13 `vlc_clone` call sites belong to the video output, audio output, interfaces, timers and similar.
  I decompiled only the input and decoder ones.

## What stays hidden

- Plugin internals.
  I did not decompile `libavcodec_plugin`, `libdirect3d11_plugin` or `libmmdevice_plugin`, so swapchain presentation, texture hand-off and WASAPI buffer sizes are unseen.
- The GPU driver and DXVA internals.
- The memory breakdown.
  I measured totals, not which part is DLL images, heap or GPU-shared memory.
- The Qt GUI, network access, adaptive streaming (HLS/DASH), DVD/Blu-ray, subtitles, Lua scripts and the update check.
  My runs used `--intf=dummy` and two local files.
- Static analysis shows what the code can do, not that every path runs.

## Measured cost of one 1080p30 file

Both runs used `--intf=dummy`, so the Qt GUI was not loaded.
A normal session costs more.

| Clip | Peak working set | Private bytes | Threads | Display surfaces | Decoder surfaces |
|---|---|---|---|---|---|
| H.264 + AAC, MP4 | 285 MB | 274 MB | 29 | 40 at 1920x1088 | 24 |
| HEVC + Opus, MKV | 298 MB | 296 MB | 29 | 46 at 1920x1080 | 27 |

At NV12 size a 1920x1088 surface is about 3.1 MB, so 40 + 24 surfaces is roughly 200 MB of video memory.
The log also reported "Decoder wait done in 2514 ms" for the MP4, measured with verbose logging on, which slows startup.

## Proposal: a small working player

The smallest honest copy of VLC's design keeps its stages and drops the plugin system.
FFmpeg already provides the access, demux, packetizer and decoder stages that VLC wraps in plugins.

```text
main thread     window, input, render loop (show frame when pts <= clock, drop if late by > half a frame)
demux thread    avformat_open_input -> av_read_frame -> bounded packet queues (video, audio)
video decoder   avcodec + d3d11va hwaccel, software fallback -> frame queue (4 frames)
audio decoder   avcodec -> swresample to the device format -> ring buffer
audio callback  WASAPI pulls samples and advances the master clock (pts minus device latency)
seek            av_seek_frame, bump a serial number, flush every queue (ffplay's method)
```

| VLC piece | Small version |
|---|---|
| access + stream filters | FFmpeg `avio` (file, http) |
| demux + packetizer | `libavformat` |
| decoder + hw decoder | `libavcodec` with the d3d11va hwaccel |
| video converter | none on the GPU path, `swscale` on the fallback path |
| vout display | SDL2 texture first, then Direct3D 11 or wgpu for zero-copy |
| audio output | WASAPI through `cpal` or SDL2 audio |
| clock | audio master clock, about 40 lines |
| Qt interface | keyboard controls and a minimal overlay |

It needs no services for local playback.
Everything VLC does over the network is optional: crash upload, update check, metadata and art lookup, subtitle search, UPnP and Chromecast discovery.
Add one only when you want that feature.

## Making it faster and lighter than VLC

1. Keep decoded frames on the GPU.
   VLC does this with d3d11va sharing the display's device.
   Copying frames back to RAM costs about 3 MB per 1080p frame and a lot more at 4K.
2. Size frame pools to what the stream needs.
   VLC allocated 40 display surfaces plus 24 decoder surfaces for a 1080p clip.
   The decoder needs its reference frames plus a few spare, and the display needs about 3.
3. Bound queues by duration and bytes, not packet count.
   One or two seconds of packets and four decoded frames is enough for local files.
4. Do no startup work before the first frame.
   VLC parses its media-library playlist before your file.
5. Link the few codecs and outputs you need instead of probing a plugin cache.
6. Limit `probesize` and `analyzeduration` so opening a file stays fast.
7. Drop frames before decoding when far behind, by skipping non-reference frames, instead of decoding them and throwing them away at display time.
8. Use a fixed set of about five threads plus FFmpeg's own decoder threads.
9. Reuse reference-counted frames (`AVFrame` refs) and avoid per-frame allocation.
10. Skip a heavy GUI toolkit.
    VLC's Qt plugin alone is about 19 MB of DLL.

## Rust or C?

Rust will not make the codecs faster.
The hot loops in FFmpeg and dav1d are hand-written SIMD assembly in either language, and rewriting them is years of work.
Rust will not cut memory on its own either, because memory goes to frame buffers, and those cost the same in any language.

Rust's real gain is safety in the code you write.
Media players parse untrusted files, and most VLC security fixes over the years have been memory bugs in demuxers and parsers.
Rust removes that class of bug from your own code, and its ownership rules make frame lifetimes and thread hand-offs explicit, which helps keep the pools tight.

My recommendation is Rust for the player, pipeline, clock and UI, with FFmpeg (C and assembly) for demux and decode through FFI.
Writing it in C only makes sense if you plan to fork or contribute to VLC itself.
The final choice is yours.

## Licences

The VLC app is GPLv2 (`COPYING.txt` in the install folder).
libVLC is LGPLv2.1, per the official LibVLC wiki page.
FFmpeg is LGPL unless built with GPL parts such as x264 or x265.
The FFmpeg build you have from winget is a GPL build, so linking it makes your player GPL.
Use an LGPL shared build if you want to keep your code closed.

## Evidence index

| ID | What it shows |
|---|---|
| `ev_b560fee2…` | `vlc.exe` PE resources: icons, version info, manifest |
| `ev_c9f13746…` | `vlc.exe` strings: the seven libvlc imports, crash URL, flags |
| `ev_9f3534c3…` | `WinMain` decompiled |
| `ev_6b78fc57…` | core log strings: module search, late pictures, audio drift |
| `ev_ec61398f…` | core exports: module, input, demux, fifo, picture pool, thread APIs |
| `ev_5be4721f…` | `vlc_module_load` decompiled |
| `ev_a3528296…` | per-capability candidate lookup |
| `ev_ba6b1c33…` | `demux_New` decompiled |
| `ev_840e18fd…` | `decoder_New` / `CreateDecoder` decompiled |
| `ev_a758fd91…` | decoder category table strings |
| `ev_050c5559…` | `input_Start` decompiled |
| `ev_759822184…` | the 15 callers of `vlc_clone` |
| `ev_99926c5d…` | `ThreadDisplayPreparePicture` decompiled |

Official sources checked: videolan.org/vlc, wiki.videolan.org pages Hacker_Guide/Core, How_To_Write_a_Module, Documentation:VLC_Modules_Loading, Hacker_Guide/Video_Output and LibVLC.
The wiki pages date from 2019 to 2021 and describe VLC 3.x.
