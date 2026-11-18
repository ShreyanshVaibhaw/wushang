# Windows catch-up progress

The goal remains parity with shared features and fixes on `origin/macos` at `7c375c5`, while retaining Windows integrations.
This is not complete.

## Implemented for Windows 1.0.5

- Bookmark loading tolerates malformed bytes, protects unreadable files, and normalizes loaded paths consistently with lookups.
- Custom preamp settings retain their full range; selecting the current profile preserves live changes.
- Normal-speed DSP avoids an output copy; visualizer samples are windowed once per frame.
- Subtitle code-page detection validates language hints and handles UTF-16 byte-order marks through native Windows decoding.
- Selecting the current shuffled queue item preserves shuffle order.
- Failed recordings and snapshots remove incomplete outputs; snapshots commit their WIC stream before reporting success.
- Players connecting as a broadcast stops are released.
- DVD IFO reads are capped.
- Library state and tag index writes run on a shared writer thread and finish before shutdown.
- Directory listings, existence checks and modification times are read off the UI thread.
- Folder queues use refreshed listings, and changes invalidate displayed pages.
- Playlist editing preserves non-UTF-8 bytes; track preferences follow rename and deletion.
- Windows playlist rename checks file identity to avoid overwriting a different file on a case-sensitive volume.
- Tag-index version 2 escapes paths and still reads version 1.
- Toolbar groups fit the window, omitted items remain available in Customize, and drops that overflow are refused.
- FFmpeg build configuration includes more WMV and WMA decoders; the DLLs have not yet been rebuilt.

## Playback tabs implemented

The Windows player now supports up to eight concurrent tabs, with independent queues, playback positions, tracks, subtitles, picture settings, volume, mute and effects.
Only the active tab is audible; inactive tabs keep decoding and advancing their queues.
The title bar has a new-tab button and a tab strip, with Ctrl+T, Ctrl+W, Ctrl+1 through Ctrl+9 and next/previous shortcuts.
Windows menus expose New Tab, Close Tab and Play in New Tab.
Native file dialogs run between render passes with a modal timer, so video continues while a picker is open.
Fonts remain isolated between sessions, and software video-frame conversion runs on decoder threads.
Explorer file-open forwarding into an already running instance remains outstanding.

## Validation so far

- Standalone queue tests: 16 passed.
- Standalone code-page tests: 6 passed.
- Standalone DSP tests: 14 passed, including the expensive pitch/length test.
- Standalone streaming tests: 2 passed.
- Shared suite before toolbar changes: 202 passed, 1 ignored, 1 filtered out.
  The filtered test is the unchanged DSP pitch/length test that passed separately.
- UI suite after toolbar changes: 49 passed.
- Suite after audio/font prerequisites: 211 passed, 1 ignored, 1 unchanged DSP test filtered out.
- Tab controller suite: 235 passed, 1 ignored, 1 unchanged DSP test filtered out.
  A Mac-specific title-bar helper test was then removed because Windows uses native nonclient handling.
- The subsequent Windows menu regression test passed, and the debug application build succeeded.
- The new audio-delay/seek-target regression passed separately after that suite.
- The newest tab-enabled debug build was exercised with ExtraCursor.
  Ctrl+T opened the native picker, Beta opened beside Alpha, Ctrl+1 and Ctrl+2 switched them, and Ctrl+W closed Beta.
  Alpha retained its paused position while Beta kept playing silently in the background.
  A second run confirmed visible video progression while the native picker stayed open, and cancellation kept the original tab.
  Both launched applications were closed.
- Formatting and diff whitespace checks passed.
- The Windows debug build succeeded; see `D:/wushang-port-review/windows-build.log`.
- The installed baseline was exercised with ExtraCursor and closed afterward.
  The modified debug application was also launched through ExtraCursor.
  Playback, the Customize palette, cancellation and returning to the library were visually checked, and the app was closed.
  Overflow edge cases are covered by unit tests; narrow-window drag behavior still needs a runtime check.

## Remaining work

Explorer file-open forwarding into an existing instance remains outstanding.
The remaining conversion, subtitle/font isolation, networking, thumbnail cache, rendering and UI fixes also need porting and verification.
The complete source comparison is in `outputs/macos-main-gap.md`.
Do not treat staged files in `D:/wushang-port-review/src` as a finished port: several contain conflict markers or Mac-specific code.
Windows 1.0.5 packages these completed changes for release.
The source comparison above records the remaining parity work separately.
The full release-app validation passed 235 automated tests, with one audible-device test ignored.
See `outputs/release-test-2026-11-18.md` for the runtime checks.
