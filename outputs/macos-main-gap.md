# Changes on macos missing from Windows main

Compared after `git fetch origin --prune` on 2026-11-17.
This is a source and history comparison; no application tests were run.

## Branch state

- Local `main`: `81b1acb`, four commits behind `origin/main`.
- Remote Windows `main`: `88cff9f`, committed 2026-11-09.
- Remote `macos`: `7c375c5`, committed 2026-11-15.
- Common ancestor: `81b1acb`.
- `macos` has 132 commits outside `origin/main`; `origin/main` has four outside `macos`.
- Three of those macOS commits have patch-equivalent Windows commits, leaving 129 patch-distinct macOS commits to inspect.
- The 129 include Mac-specific implementation, documentation and refactoring, so they are not 129 missing Windows features.

## Windows catch-up list

These groups identify behavior or implementation changes on macOS that merit a Windows port or equivalent check.
The commit references identify the relevant changes, not a promise that each commit applies cleanly to Windows.
Some later commits depend on earlier macOS refactoring.

1. **Playback tabs.**
   Up to eight files playing concurrently, only the active tab audible, independent queues, tracks, subtitles, position, picture settings, volume, mute and effects, plus tab controls and shortcuts.
   References: `9f5bd63`, `f5626fc`, `55bc00d`, `0564e29`, `781a5be`, `686d6e5`, `667f528`, `125022e`.
   Finder opening files in a new tab (`461377e`) needs a Windows-specific equivalent.

2. **Conversion and recording reliability.**
   Preserve audio/video sync and resampled audio, handle changing video dimensions, cancel dropped jobs, avoid hanging on quit, preserve output being finalized, and clean up failed recordings or snapshots.
   References: `24c6be2`, `115e16b`, `d7d5121`, `f94c13f`, `ab72fc6`, `6785274`, `3d1a7fb`, `98bd826`.

3. **Queues and connecting streams.**
   Skip files that cannot open, end queues containing only missing files, retain additions while a stream connects, refresh folder-derived queues, refresh pages when those queues discover changed contents, and fix drag cleanup and shuffle behavior.
   References: `24c6be2`, `8541f4e`, `4327b01`, `ae787b2`, `987c1c8`, `54daa73`, `b0f1236`, `7c375c5`, `a5d1df9`, `249288f`.

4. **Responsive library and safer persistence.**
   Save library state and tags in the background, list directories and read sort timestamps off the UI thread, index newly added Music folders, retain folders with deeper media, escape tag-index paths, and preserve symlink-sensitive paths.
   References: `358e36d`, `702b803`, `561324d`, `2085bf3`, `bf5db72`, `696d112`, `41b25a4`, `cc25732`, `12164fc`.
   Port the final path behavior including `12164fc`, which corrects the earlier normalization.

5. **Playlist and saved-state correctness.**
   Preserve track preferences when renaming or deleting playlists, edit non-UTF-8 playlists safely, reject case-colliding playlist renames, retain bookmarks with malformed bytes, preserve paused-seek positions, and avoid resetting an already selected profile.
   References: `310189e`, `acc24f1`, `e27d4ea`, `24c6be2`.

6. **Subtitle handling.**
   Recognize UTF-16 byte-order marks, allow picture subtitles beyond text limits, validate language hints, distinguish cached subtitle images by pixels, preserve file-opening policy during preroll, and accept dropped subtitle files.
   References: `4f9690d`, `5b6397f`, `af9c4d5`, `f9d369d`, `35cbfd8`, `868f4e0`, `24c6be2`.

7. **Network feeds and stream lifecycle.**
   Add overall fetch deadlines, allow podcast feeds up to 32 MB, resolve relative feed links against the final feed address, share URL parsing, and release players that finish connecting after cancellation.
   References: `d71b7ea`, `e22fcb4`, `ff66bf2`, `7f21984`, `5ed826d`.

8. **Audio and playback correctness.**
   Keep audio delay from shifting seeks, play audio when video decoding fails, stop reverse playback at sleep-timer expiry, fall back to the default output and ramp gain changes, retain a custom preamp above +8 dB, and support a +20 dB preamp.
   References: `24c6be2`, `33d6217`, `52b44ae`, `d1a3d17`, `a73f736`, `e77cd20`, `5846f34`.
   Check device behavior against Windows' WASAPI backend before porting.

9. **UI layout, menus and input.**
   Bound scrolling, improve library search editing and focus, keep menus inside short windows, hide the queue with the rest of the UI, end stale drags, omit toolbar items that do not fit, expose omitted items in Customize, and reject drops without room.
   References: `ea8d22d`, `7313f18`, `c4d7183`, `659b45f`, `d0f7d30`, `b8d071f`, `783777f`, `c34766f`, `597917a`, `6d946f8`, `1cb6e7e`, `cf6de29`.
   Fullscreen layout changes (`1700b21`, `c143379`) need checking against Windows fullscreen behavior.
   Command/Option and input-method fixes include Mac-specific paths.

10. **Thumbnail performance and shutdown.**
    Bound retained thumbnails, preserve visible cached thumbnails, retry failures, count all packets toward decode limits, and avoid waiting on thumbnail workers at quit.
    References: `48e4476`, `a284417`, `60c779d`, `289168c`, `3288bf2`.

11. **Rendering and shared-code cleanup.**
    Align chroma in odd-sized frames, move software-frame repacking to decoder workers, reduce audio and cached-text copies, window visualizer samples once per frame, and consolidate bitmap, text, icon, sort and playback helpers.
    References: `a1fba06`, `66713a3`, `9441013`, `dc6723d`, `9c6e5ca`, `6500193`, `e57f74b`, `4a7aee8`, `d1163f0`, `1c2123d`, `d550de4`.
    Metal and Core Text changes need Windows equivalents rather than direct replacement of Direct3D or DirectWrite code.

12. **Disc limits and codec coverage to review.**
    Bound IFO and playlist reads (`fe80bb5`).
    Review Blu-ray play-item interval handling (`814e365`) against Windows' existing libbluray path, because macOS uses its own reader.
    The macOS FFmpeg build adds WMV 7/8 and additional WMA decoders (`e45b39c`); Windows' current build script lists `wmv3` and `wmav2` but lacks that expanded decoder set.
    Equivalent decoder coverage needs a Windows build-script change rather than copying the macOS script.

## Mac-specific work to keep separate

AppKit, Metal, VideoToolbox, EDR, Core Graphics, Core Text, AVFoundation capture, Finder, Dock, Launch Services, macOS folder locations, disk-image mounting, update quarantine, app signing, DMG packaging, Homebrew settings and Mac shortcut conventions belong to the macOS port.
Do not merge the branch wholesale into Windows: the port replaces Windows platform implementations and dependencies.
Windows MSI installation, registry associations, Direct3D rendering and update installation must be preserved when bringing shared changes over.

## Fixes already present on remote Windows main

| macOS commit | Windows equivalent | Change |
|---|---|---|
| `3ff0a02` | `2486b99` | Bound decoded subtitle-picture memory |
| `5e38ca1` | `16651ba` | Prevent quadratic malformed-feed entity parsing |
| `7da052c` | `a1a7906` | Keep stream addresses playable in playlists |

The fourth Windows-only commit is merge commit `88cff9f`.
These three fixes are missing from local `main` until it is updated, but are already on `origin/main`.

## Latest macOS changes

The newest commit is `7c375c5`, "Redo a page whose listing a play queue found changed", committed 2026-11-15 at 21:52:01 +05:30.
It refreshes an already displayed library page when queue construction discovers a changed directory listing, and adds a regression assertion.

The most recent ten commits, newest first:

- `7c375c5` Redo a page whose listing a play queue found changed
- `6785274` Never delete a conversion that is writing out its finished file
- `54daa73` Keep files added to a connecting stream's queue in all cases
- `cf6de29` Keep a left-out bar item when its palette cell is clicked
- `1cb6e7e` Show bar items the window has no room for in Customize
- `c143379` Lay the window out again when fullscreen comes or goes
- `a5d1df9` End a queue row's drag when the pointer's work is ended
- `a284417` Keep on-screen thumbnails cached, and retry unreadable ones
- `12164fc` Keep a `..` that follows a symlink in library paths
- `ab72fc6` Don't let a stuck conversion hold up quitting

## Complete macOS-only commit list

Newest first.

The status column comes from `git cherry origin/main origin/macos`.

| Commit | Date | Status | Subject |
|---|---|---|---|
| [7c375c5](https://github.com/ShreyanshVaibhaw/wushang/commit/7c375c532c1b1f4303dbabf3c301757f431eaf4d) | 2026-11-15 | Patch-distinct | Redo a page whose listing a play queue found changed |
| [6785274](https://github.com/ShreyanshVaibhaw/wushang/commit/6785274d49d30bec18edc43b872314bcfaf1c849) | 2026-11-15 | Patch-distinct | Never delete a conversion that is writing out its finished file |
| [54daa73](https://github.com/ShreyanshVaibhaw/wushang/commit/54daa731a498b08b8cc2981b00182a1f18ac212b) | 2026-11-15 | Patch-distinct | Keep files added to a connecting stream's queue in all cases |
| [cf6de29](https://github.com/ShreyanshVaibhaw/wushang/commit/cf6de29e457b50b777e602c53bd8ee5cb63796f3) | 2026-11-15 | Patch-distinct | Keep a left-out bar item when its palette cell is clicked |
| [1cb6e7e](https://github.com/ShreyanshVaibhaw/wushang/commit/1cb6e7eecb30aace8a1d1846f3bdb436359e11ed) | 2026-11-15 | Patch-distinct | Show bar items the window has no room for in Customize |
| [c143379](https://github.com/ShreyanshVaibhaw/wushang/commit/c143379387460a9a5ec38bd1e60c836b42702971) | 2026-11-15 | Patch-distinct | Lay the window out again when fullscreen comes or goes |
| [a5d1df9](https://github.com/ShreyanshVaibhaw/wushang/commit/a5d1df9472a8f91f65d5fbd29cf61fc07992a9fa) | 2026-11-15 | Patch-distinct | End a queue row's drag when the pointer's work is ended |
| [a284417](https://github.com/ShreyanshVaibhaw/wushang/commit/a2844179e7156691cfe91cb01a7b32fe7dd10c08) | 2026-11-15 | Patch-distinct | Keep on-screen thumbnails cached, and retry unreadable ones |
| [12164fc](https://github.com/ShreyanshVaibhaw/wushang/commit/12164fcbe120518f11dacaaff4b06b7ef783695e) | 2026-11-15 | Patch-distinct | Keep a `..` that follows a symlink in library paths |
| [ab72fc6](https://github.com/ShreyanshVaibhaw/wushang/commit/ab72fc65f172344f7c29db548e8ac809a179a961) | 2026-11-15 | Patch-distinct | Don't let a stuck conversion hold up quitting |
| [b0f1236](https://github.com/ShreyanshVaibhaw/wushang/commit/b0f12362feeee8b6aa51f2165ec841bbaa021c39) | 2026-11-15 | Patch-distinct | Make play queues from the folder as it is now |
| [5846f34](https://github.com/ShreyanshVaibhaw/wushang/commit/5846f3496ce2b0740a10678968adf822df47cfa8) | 2026-11-15 | Patch-distinct | Let the device test go on when an output fails to open |
| [987c1c8](https://github.com/ShreyanshVaibhaw/wushang/commit/987c1c8b13f78ab91f932dd9decb0db452e2afb7) | 2026-11-15 | Patch-distinct | Treat a stream still connecting as open for drops and the queue |
| [4327b01](https://github.com/ShreyanshVaibhaw/wushang/commit/4327b0172f936aecdcd66b9c3278c31f798f0e93) | 2026-11-15 | Patch-distinct | End a queue of missing files that open in the background |
| [e637042](https://github.com/ShreyanshVaibhaw/wushang/commit/e637042d7646b4ed8e70b9e7a21b3fdb94de9203) | 2026-11-15 | Patch-distinct | Recolour the queue's and Settings' icons through recolor |
| [6d946f8](https://github.com/ShreyanshVaibhaw/wushang/commit/6d946f84b167f689b329c3f143070223c69e0a6f) | 2026-11-15 | Patch-distinct | Refuse a Customize drop the window has no room for |
| [d550de4](https://github.com/ShreyanshVaibhaw/wushang/commit/d550de40d024063ee22b3ad0ab5dddb20d21a823) | 2026-11-15 | Patch-distinct | Play a page's entry through one helper, and share two others |
| [1c2123d](https://github.com/ShreyanshVaibhaw/wushang/commit/1c2123de87468dec08dc2884dba1e77e3f2e06b0) | 2026-11-15 | Patch-distinct | Share the natural sort and the shuffle, and drop insert_next |
| [2085bf3](https://github.com/ShreyanshVaibhaw/wushang/commit/2085bf30802656356765b72e33fe18928c262fdd) | 2026-11-15 | Patch-distinct | Save the tag index on the library's writer thread |
| [a73f736](https://github.com/ShreyanshVaibhaw/wushang/commit/a73f7368e397ed3d972e513530f698f182ad7790) | 2026-11-15 | Patch-distinct | Keep a custom preamp above +8 dB across restarts |
| [bf5db72](https://github.com/ShreyanshVaibhaw/wushang/commit/bf5db72ee7d730380d19293823873b0367a442b6) | 2026-11-15 | Patch-distinct | Index a Music folder added while running |
| [ae787b2](https://github.com/ShreyanshVaibhaw/wushang/commit/ae787b2379eff5124c8f959f067e5e5e050a1914) | 2026-11-15 | Patch-distinct | Add to the queue of a stream that's still connecting |
| [52b44ae](https://github.com/ShreyanshVaibhaw/wushang/commit/52b44ae681c231b9d396c1eb8382e1c65e487956) | 2026-11-15 | Patch-distinct | Stop reverse playback when the sleep timer runs out |
| [868f4e0](https://github.com/ShreyanshVaibhaw/wushang/commit/868f4e0c17e8339b69bfbefe860dd1ee16f162b1) | 2026-11-15 | Patch-distinct | Add a subtitle file dropped on the window to the video |
| [8541f4e](https://github.com/ShreyanshVaibhaw/wushang/commit/8541f4e4facb9711db158822bb8728c8404717e8) | 2026-11-15 | Patch-distinct | Pass over queue files that can't open |
| [597917a](https://github.com/ShreyanshVaibhaw/wushang/commit/597917a5a52931c98d2dd4ea0a3a0340c5fc8cbf) | 2026-11-15 | Patch-distinct | Check the default control bar fits the smallest window |
| [28013a9](https://github.com/ShreyanshVaibhaw/wushang/commit/28013a9163b2da87bae06e6f7d32a5a9a22e4085) | 2026-11-15 | Patch-distinct | Drop the cursor ids nothing names any more |
| [d0f7d30](https://github.com/ShreyanshVaibhaw/wushang/commit/d0f7d30a1075e91b7c29b0295ae2c7d7996d5886) | 2026-11-15 | Patch-distinct | Keep a button's menu off the button in a short window |
| [4b7de2a](https://github.com/ShreyanshVaibhaw/wushang/commit/4b7de2ab2786a10799af769e2460caddaa251d46) | 2026-11-15 | Patch-distinct | Make render_check's pixel_at a private test helper |
| [32c3235](https://github.com/ShreyanshVaibhaw/wushang/commit/32c32351f11733c7e891bb7bfbb21fd9eb2c1c5b) | 2026-11-15 | Patch-distinct | Drop Windows wording left in the UI code |
| [b189922](https://github.com/ShreyanshVaibhaw/wushang/commit/b189922fa909d39e23353ac580177952d55b8906) | 2026-11-15 | Patch-distinct | Take a plain control in the popup builder's rows |
| [d1163f0](https://github.com/ShreyanshVaibhaw/wushang/commit/d1163f0e4031aa04edfb55e8f996b1a906220956) | 2026-11-15 | Patch-distinct | Parse the logo and volume meter icons once |
| [4a7aee8](https://github.com/ShreyanshVaibhaw/wushang/commit/4a7aee8d3f44abdb03b673e6964d3021199b0a12) | 2026-11-15 | Patch-distinct | Recolour icons through one helper |
| [e57f74b](https://github.com/ShreyanshVaibhaw/wushang/commit/e57f74b74ae6e564aadd6d694ee706c7853eb63a) | 2026-11-15 | Patch-distinct | Draw text through Painter::put alone |
| [6500193](https://github.com/ShreyanshVaibhaw/wushang/commit/650019386f6f02b35302bbc6e2ac59a183159b54) | 2026-11-15 | Patch-distinct | Share the BGRA bitmap code of thumbnails and seek stills |
| [7ee1093](https://github.com/ShreyanshVaibhaw/wushang/commit/7ee1093dd3a78ddbfacc24da81582daf553b2a8d) | 2026-11-15 | Patch-distinct | Make the scene's panel data a plain reference |
| [249288f](https://github.com/ShreyanshVaibhaw/wushang/commit/249288f7a4cbf26b5ed5b99ccd96526108bfcf05) | 2026-11-15 | Patch-distinct | Send the menu's Repeat and Shuffle as queue actions |
| [48e4476](https://github.com/ShreyanshVaibhaw/wushang/commit/48e44769f5cf1ee7cabf2c045428e56c53f95259) | 2026-11-15 | Patch-distinct | Keep only the thumbnails drawn lately |
| [c34766f](https://github.com/ShreyanshVaibhaw/wushang/commit/c34766fdc953d0758baaa89b228822fd9f5023eb) | 2026-11-15 | Patch-distinct | Leave out bar items that don't fit the window |
| [1700b21](https://github.com/ShreyanshVaibhaw/wushang/commit/1700b213c26cb4e5b00f8b3917bc786db00be1f1) | 2026-11-15 | Patch-distinct | Leave minimize and maximize out of the title bar in fullscreen |
| [9cb358b](https://github.com/ShreyanshVaibhaw/wushang/commit/9cb358b8551deefc8f595e59e4d0ab1fee7d81ea) | 2026-11-15 | Patch-distinct | Build the player's window buttons from the shared list |
| [783777f](https://github.com/ShreyanshVaibhaw/wushang/commit/783777f491f64180c7a0b4941e8f37bfef453899) | 2026-11-15 | Patch-distinct | End a drag when the page changes or the bars hide |
| [b8d071f](https://github.com/ShreyanshVaibhaw/wushang/commit/b8d071f909f594c56062468a993a630f00785449) | 2026-11-15 | Patch-distinct | Close the queue with the rest of the UI on Hide UI |
| [659b45f](https://github.com/ShreyanshVaibhaw/wushang/commit/659b45fccaaccb0c09248c400396d59dc9255564) | 2026-11-15 | Patch-distinct | Open a button's menu above it when there's no room below |
| [c4d7183](https://github.com/ShreyanshVaibhaw/wushang/commit/c4d718372a2aabedf801d1e50cf06be3ddbe3419) | 2026-11-15 | Patch-distinct | Give the player its keys back after a library search |
| [7313f18](https://github.com/ShreyanshVaibhaw/wushang/commit/7313f18fbb21158389d772628f56794b3b4bc681) | 2026-11-15 | Patch-distinct | Make the library search box a text field |
| [08b6156](https://github.com/ShreyanshVaibhaw/wushang/commit/08b615640aedbb02f93c4acc1b5a7fce9c569a6d) | 2026-11-15 | Patch-distinct | Drop the Customize dialog's Box and Zone targets |
| [c46f829](https://github.com/ShreyanshVaibhaw/wushang/commit/c46f829363f3148e9d69a97afe60bdd37d5aeba4) | 2026-11-15 | Patch-distinct | Keep one copy of duration_text and centre |
| [c6638bf](https://github.com/ShreyanshVaibhaw/wushang/commit/c6638bf2d39919791931b0e6bd18bcc383d55726) | 2026-11-15 | Patch-distinct | Have dialog_cursor name the pointer it wants |
| [b744c96](https://github.com/ShreyanshVaibhaw/wushang/commit/b744c96ecd92e7ce458bab7896ebab313d13efbc) | 2026-11-15 | Patch-distinct | Remove the Windows updater's leftovers |
| [561324d](https://github.com/ShreyanshVaibhaw/wushang/commit/561324d484d083da2b7eec14b4827839455797e8) | 2026-11-15 | Patch-distinct | Read the date sorts' times off the UI thread |
| [ea8d22d](https://github.com/ShreyanshVaibhaw/wushang/commit/ea8d22d4917336008a4cd450254c7f1cde37ab6d) | 2026-11-15 | Patch-distinct | Keep the library and Settings from scrolling past their end |
| [c4e938d](https://github.com/ShreyanshVaibhaw/wushang/commit/c4e938db848ef1fcae273dac4cb8e7904276d66f) | 2026-11-15 | Patch-distinct | Let the Settings search box cut, copy and paste |
| [23d844a](https://github.com/ShreyanshVaibhaw/wushang/commit/23d844ac7c3fc03c0f6f0b0b7163240340956329) | 2026-11-15 | Patch-distinct | Tell Command from Option in text fields |
| [5bffce9](https://github.com/ShreyanshVaibhaw/wushang/commit/5bffce960c7b0a26304a8abbb330ccbd34b93892) | 2026-11-15 | Patch-distinct | Drop Windows' Insert and Shift+Delete clipboard keys from text fields |
| [7618ade](https://github.com/ShreyanshVaibhaw/wushang/commit/7618ade050c26cc07a8d53f7d94408415ffe3abe) | 2026-11-15 | Patch-distinct | Let a dialog take text typed through an input method |
| [9441013](https://github.com/ShreyanshVaibhaw/wushang/commit/94410137a91cc857ff0145473293e2031c34f10e) | 2026-11-15 | Patch-distinct | Copy decoded audio less on its way to the device |
| [9c6e5ca](https://github.com/ShreyanshVaibhaw/wushang/commit/9c6e5ca1b66aa9f228d2118f8726e78ffc2de436) | 2026-11-15 | Patch-distinct | Window the visualizer's samples once per frame |
| [e77cd20](https://github.com/ShreyanshVaibhaw/wushang/commit/e77cd201d31b7e4eabac7853d0db8f881f28bcc5) | 2026-11-15 | Patch-distinct | Let the preamp reach 20 dB above unity |
| [dc6723d](https://github.com/ShreyanshVaibhaw/wushang/commit/dc6723d1801483906ee2b00e20ff0476d3c89122) | 2026-11-15 | Patch-distinct | Look up cached text without copying it |
| [66713a3](https://github.com/ShreyanshVaibhaw/wushang/commit/66713a328596f4d659fc0ba26905df0c99bde77e) | 2026-11-15 | Patch-distinct | Leave repacking software frames to the decoder thread |
| [b55048f](https://github.com/ShreyanshVaibhaw/wushang/commit/b55048f402ed34b6ff2b3c18804d9ca4e46168d1) | 2026-11-15 | Patch-distinct | Remove the renderer's no-op device lock |
| [66b76e7](https://github.com/ShreyanshVaibhaw/wushang/commit/66b76e74b8df089287677cd4356c54f170ff1d7d) | 2026-11-15 | Patch-distinct | Stop depending on objc2-io-surface directly |
| [318316e](https://github.com/ShreyanshVaibhaw/wushang/commit/318316e2c14c339ae0b06dc21379af36d86ebf74) | 2026-11-15 | Patch-distinct | Drop the Direct2D shim's unused API surface |
| [a1fba06](https://github.com/ShreyanshVaibhaw/wushang/commit/a1fba06578a3128a945807a83885bac6f3c9f012) | 2026-11-15 | Patch-distinct | Line chroma up with luma in odd-sized frames |
| [208078e](https://github.com/ShreyanshVaibhaw/wushang/commit/208078e3fefeed13111438bc852537e8ef733509) | 2026-11-15 | Patch-distinct | Repaint a window that comes back into view |
| [9b18c9f](https://github.com/ShreyanshVaibhaw/wushang/commit/9b18c9f026190148da4aa8be92a10700d45e3dbd) | 2026-11-15 | Patch-distinct | Run the modal tick at the screen's refresh rate |
| [ab62029](https://github.com/ShreyanshVaibhaw/wushang/commit/ab62029d8f0c2c48979daad1355d35222797bc9d) | 2026-11-15 | Patch-distinct | Keep Quit on in the menu bar while the window has the keys |
| [3288bf2](https://github.com/ShreyanshVaibhaw/wushang/commit/3288bf2048425bbc663847dfc21d3770174e1310) | 2026-11-15 | Patch-distinct | Don't wait for thumbnail workers when quitting |
| [60c779d](https://github.com/ShreyanshVaibhaw/wushang/commit/60c779d2b06a6ed864b2e34db4db77edb9894803) | 2026-11-15 | Patch-distinct | Try a failed thumbnail again when it is asked for again |
| [289168c](https://github.com/ShreyanshVaibhaw/wushang/commit/289168c4b431f21a2f9d6274bb49adafa9d2daec) | 2026-11-15 | Patch-distinct | Count every packet toward the thumbnail's packet limit |
| [cc25732](https://github.com/ShreyanshVaibhaw/wushang/commit/cc257328cdd5e80d1e777ead96dacfe4304c1908) | 2026-11-15 | Patch-distinct | Take .. out of library paths |
| [e27d4ea](https://github.com/ShreyanshVaibhaw/wushang/commit/e27d4ea8398cbd086415d198d5b57323666b9fe1) | 2026-11-15 | Patch-distinct | Refuse renaming a playlist onto another that differs only in case |
| [acc24f1](https://github.com/ShreyanshVaibhaw/wushang/commit/acc24f1d534dbe9f15a17f29ca0997a02d709808) | 2026-11-15 | Patch-distinct | Edit playlists that aren't UTF-8 without changing their other lines |
| [702b803](https://github.com/ShreyanshVaibhaw/wushang/commit/702b8034b83d8aae1b967ffe15008970f0d0e495) | 2026-11-15 | Patch-distinct | List folders and check Home's files off the UI thread |
| [358e36d](https://github.com/ShreyanshVaibhaw/wushang/commit/358e36d7a0e2c197a47b8069ad21542942ef7e7a) | 2026-11-15 | Patch-distinct | Save the library on a background thread |
| [696d112](https://github.com/ShreyanshVaibhaw/wushang/commit/696d112f2f719bee98334d2e511c0bbc721919d1) | 2026-11-15 | Patch-distinct | Keep folders whose media is deeper than the search listed |
| [310189e](https://github.com/ShreyanshVaibhaw/wushang/commit/310189ebe9b9c7b4a01c79dc38e9b9ea69b6c193) | 2026-11-15 | Patch-distinct | Move a playlist's track choices with it on rename and delete |
| [fe80bb5](https://github.com/ShreyanshVaibhaw/wushang/commit/fe80bb5d1895c14157c4689fa7a30be8adb78f8f) | 2026-11-15 | Patch-distinct | Read disc IFOs and playlists no further than their limit |
| [41b25a4](https://github.com/ShreyanshVaibhaw/wushang/commit/41b25a47127e3b701909d440fc9c121a1d0b2430) | 2026-11-15 | Patch-distinct | Escape paths in the tag index |
| [18ffd90](https://github.com/ShreyanshVaibhaw/wushang/commit/18ffd90d4c8069bdc0177a5dd3f33425c40f3601) | 2026-11-15 | Patch-distinct | Read stream tags with thumbs::tag in the Properties probe |
| [33d6217](https://github.com/ShreyanshVaibhaw/wushang/commit/33d6217e03ef981efa3c4269b9c655b91e64d79d) | 2026-11-15 | Patch-distinct | Play the sound when a file's video can't be decoded |
| [35cbfd8](https://github.com/ShreyanshVaibhaw/wushang/commit/35cbfd8dae74fec7890bf6d055186b0b62b67457) | 2026-11-15 | Patch-distinct | Reopen files for the subtitle preroll as they were first opened |
| [3de862d](https://github.com/ShreyanshVaibhaw/wushang/commit/3de862d29d80f371676779618518ab994828987e) | 2026-11-15 | Patch-distinct | Quarantine the downloaded update |
| [7f21984](https://github.com/ShreyanshVaibhaw/wushang/commit/7f219844e8816ebb50ac89307d3f27759dad4459) | 2026-11-15 | Patch-distinct | Take addresses apart in one place |
| [ff66bf2](https://github.com/ShreyanshVaibhaw/wushang/commit/ff66bf24ed710ead40a61184beda7b3aa8990495) | 2026-11-15 | Patch-distinct | Resolve feed links against the feed's path and final address |
| [e22fcb4](https://github.com/ShreyanshVaibhaw/wushang/commit/e22fcb491a560ed876d7e225756f20d8f1ea5772) | 2026-11-15 | Patch-distinct | Take podcast feeds up to 32 MB |
| [d71b7ea](https://github.com/ShreyanshVaibhaw/wushang/commit/d71b7eaa4d757d65deda3e346f230e1c01145de9) | 2026-11-15 | Patch-distinct | Give feed and update fetches an overall deadline |
| [f9d369d](https://github.com/ShreyanshVaibhaw/wushang/commit/f9d369d92233c9d81408088ff90939c0f5a85a3b) | 2026-11-15 | Patch-distinct | Know cached subtitle pictures by their pixels as well as their start |
| [af9c4d5](https://github.com/ShreyanshVaibhaw/wushang/commit/af9c4d52ccb4d2c6ac3900b7e89d6c518a5d313f) | 2026-11-15 | Patch-distinct | Ignore language tags that don't fit the subtitle text |
| [5b6397f](https://github.com/ShreyanshVaibhaw/wushang/commit/5b6397ffdd70ec8d9e3eef25643c5656009ff0b6) | 2026-11-15 | Patch-distinct | Let picture subtitle tracks past the text size limit |
| [4f9690d](https://github.com/ShreyanshVaibhaw/wushang/commit/4f9690d5938857039700751c3e126bfcd6a9d260) | 2026-11-15 | Patch-distinct | Read UTF-16 subtitle files by their byte order mark |
| [5ed826d](https://github.com/ShreyanshVaibhaw/wushang/commit/5ed826d516337ca9b440efa2be716161477e1548) | 2026-11-15 | Patch-distinct | Let go of players that connect as the stream stops |
| [86aad53](https://github.com/ShreyanshVaibhaw/wushang/commit/86aad53e6d8cf7a51284314d9186aee28f212150) | 2026-11-15 | Patch-distinct | Stop renaming streams titled like Windows devices |
| [98bd826](https://github.com/ShreyanshVaibhaw/wushang/commit/98bd8266ec0c8e56d6135dcb82c77abe401d2d2b) | 2026-11-15 | Patch-distinct | Report and clean up a snapshot that fails to write |
| [3d1a7fb](https://github.com/ShreyanshVaibhaw/wushang/commit/3d1a7fbc9468f36166abc8d80acf5063235a4e62) | 2026-11-15 | Patch-distinct | Remove the file of a recording that can't start |
| [f94c13f](https://github.com/ShreyanshVaibhaw/wushang/commit/f94c13fcd73cc0b84b79fe42c3251b6f4db7445a) | 2026-11-15 | Patch-distinct | Convert a picture that changes shape midway |
| [d7d5121](https://github.com/ShreyanshVaibhaw/wushang/commit/d7d512156e6111d3f49427dce1936d9bdcee8387) | 2026-11-15 | Patch-distinct | Cancel a conversion when its job is dropped |
| [115e16b](https://github.com/ShreyanshVaibhaw/wushang/commit/115e16b7565bfe715f6c521e7b37d00394ffcde4) | 2026-11-15 | Patch-distinct | Keep converted sound in step with the picture |
| [24c6be2](https://github.com/ShreyanshVaibhaw/wushang/commit/24c6be22bda478743c397bf71e5240fb5a75a814) | 2026-11-15 | Patch-distinct | Fix fifteen bugs found in the code audit |
| [461377e](https://github.com/ShreyanshVaibhaw/wushang/commit/461377eb38c7c0e9f62ad7317c95de15271af40c) | 2026-11-15 | Patch-distinct | Open files from Finder in a new tab |
| [328384d](https://github.com/ShreyanshVaibhaw/wushang/commit/328384d6b318f74d59a696f68e28b254a1b2a4a7) | 2026-11-10 | Patch-distinct | Read Command shortcuts from the layout's Command keys |
| [125022e](https://github.com/ShreyanshVaibhaw/wushang/commit/125022ee7470915462dabdb8675925a0e738ed31) | 2026-11-10 | Patch-distinct | Save every tab before logging out |
| [b2fc30d](https://github.com/ShreyanshVaibhaw/wushang/commit/b2fc30dc938856e648fb5dbb77904cbb04c77b9d) | 2026-11-10 | Patch-distinct | Build the pump's end check only where the debug remote uses it |
| [667f528](https://github.com/ShreyanshVaibhaw/wushang/commit/667f52898929558712d9c1e9286a1b2bea893812) | 2026-11-10 | Patch-distinct | Show an empty tab at the start, not counting on |
| [781a5be](https://github.com/ShreyanshVaibhaw/wushang/commit/781a5be1ec5a3066025731a3f48c457f17ce8614) | 2026-11-10 | Patch-distinct | Keep tabs apart, and every tab within reach |
| [686d6e5](https://github.com/ShreyanshVaibhaw/wushang/commit/686d6e5161b68f60a961084f6e93dca7a465be53) | 2026-11-10 | Patch-distinct | Choose a narrow tab's title or number by the room left |
| [496e89e](https://github.com/ShreyanshVaibhaw/wushang/commit/496e89ec4a89ab45f7ae114f2db4faa6eb48fd90) | 2026-11-10 | Patch-distinct | Read the top row as digits on AZERTY |
| [d1a3d17](https://github.com/ShreyanshVaibhaw/wushang/commit/d1a3d172d8ee9dd11c45f3a6ad8de1baca6c9a5e) | 2026-11-10 | Patch-distinct | Fall back to the default output, and ramp gain changes |
| [0564e29](https://github.com/ShreyanshVaibhaw/wushang/commit/0564e297d9bb74a33784559bfcb55b7afdc317ab) | 2026-11-10 | Patch-distinct | Keep each tab's volume, mute and sound effects its own |
| [5ea66d1](https://github.com/ShreyanshVaibhaw/wushang/commit/5ea66d1053facdebcf64fe96bd13264f2b4753e7) | 2026-11-10 | Patch-distinct | Describe tabs in the README |
| [55bc00d](https://github.com/ShreyanshVaibhaw/wushang/commit/55bc00d6d17d22f89d0269d65fc4a096f0c439e4) | 2026-11-10 | Patch-distinct | Play several files at once in tabs |
| [f5626fc](https://github.com/ShreyanshVaibhaw/wushang/commit/f5626fcc85a3fb27d1b8d73d02f205068845b0bb) | 2026-11-10 | Patch-distinct | Draw the open tabs in the player's title bar |
| [6a8046a](https://github.com/ShreyanshVaibhaw/wushang/commit/6a8046a3174bf2e192ec2144b3d81e05352cb1a2) | 2026-11-10 | Patch-distinct | Allow for output latency in the external audio sync test |
| [9f5bd63](https://github.com/ShreyanshVaibhaw/wushang/commit/9f5bd63736c2481657ab1651cdc82a91150cb42b) | 2026-11-10 | Patch-distinct | Add the interface between tabbed playback and the title bar |
| [e45b39c](https://github.com/ShreyanshVaibhaw/wushang/commit/e45b39c1e85b90bb546f6f1fbd498de0e14deabb) | 2026-11-09 | Patch-distinct | Play WMV 7 and 8 files and every kind of WMA sound |
| [814e365](https://github.com/ShreyanshVaibhaw/wushang/commit/814e3654317b16026b5583de8155c87eff93743a) | 2026-11-08 | Patch-distinct | Play only each Blu-ray play item's in/out interval |
| [7da052c](https://github.com/ShreyanshVaibhaw/wushang/commit/7da052c686b67345ff6abc3b99d63973681f11ec) | 2026-11-08 | Already applied under another hash | Keep stream addresses in playlists playable |
| [5e38ca1](https://github.com/ShreyanshVaibhaw/wushang/commit/5e38ca1f16722bdda62fc5cbc3089d809df16749) | 2026-11-08 | Already applied under another hash | Stop malformed feed entities taking quadratic time |
| [3ff0a02](https://github.com/ShreyanshVaibhaw/wushang/commit/3ff0a02a8072c1e1355091f8187219a6bca79542) | 2026-11-08 | Already applied under another hash | Bound the memory decoded subtitle pictures can take |
| [02af39a](https://github.com/ShreyanshVaibhaw/wushang/commit/02af39ab052d073108ecae47b3900295bdaf6b4d) | 2026-11-08 | Patch-distinct | Load on macOS 13 without Core Audio's tap symbols, and say why a file didn't open |
| [c13f264](https://github.com/ShreyanshVaibhaw/wushang/commit/c13f26437218c6aea32d8ee63a87b4cff2841f7b) | 2026-11-03 | Patch-distinct | Say in the README that launching the build copy lists it again |
| [5125ada](https://github.com/ShreyanshVaibhaw/wushang/commit/5125adafdcf5de2e2d57390363b46b3b83396ea1) | 2026-11-03 | Patch-distinct | Keep build copies of the app out of Spotlight and Open With |
| [473e2a1](https://github.com/ShreyanshVaibhaw/wushang/commit/473e2a1e5626295375d418bb9e8ae6aff5daf9ba) | 2026-11-03 | Patch-distinct | Decode PNG cover art, and make the porting notes a maintainer guide |
| [2914b8b](https://github.com/ShreyanshVaibhaw/wushang/commit/2914b8b4ffa8fefb03f20e8b440ebf02fe4ffb4e) | 2026-11-03 | Patch-distinct | Keep the release build free of the debug remote's window helper |
| [ed10918](https://github.com/ShreyanshVaibhaw/wushang/commit/ed109185b9456474857019fefc53fcaa239cc989) | 2026-11-03 | Patch-distinct | Build slim LGPL FFmpeg and libass for macOS 13, and bundle them |
| [f2b3458](https://github.com/ShreyanshVaibhaw/wushang/commit/f2b345882d852223c28b1711a0c336bbbc894d23) | 2026-11-03 | Patch-distinct | Fix menu-bar keys over dialogs, Finder opens and macOS wording |
| [46ab155](https://github.com/ShreyanshVaibhaw/wushang/commit/46ab155e24b3961b66fa380ba27a350f715f0da9) | 2026-11-03 | Patch-distinct | Rewrite the README for the macOS app |
| [a9037d6](https://github.com/ShreyanshVaibhaw/wushang/commit/a9037d6b2f6c42e7f960c889fbebe99f4fa7f752) | 2026-11-03 | Patch-distinct | Keep video playing while a file picker is open |
| [bbbd1a2](https://github.com/ShreyanshVaibhaw/wushang/commit/bbbd1a2fb77a133831a40f07c6d3acaa3169b7bd) | 2026-11-03 | Patch-distinct | Name discs by their volume label in the Open Disc menu |
| [5be231b](https://github.com/ShreyanshVaibhaw/wushang/commit/5be231b81d26a7ff139ae35672914882df329fe1) | 2026-11-03 | Patch-distinct | Port wushang to macOS |
| [48fbd7e](https://github.com/ShreyanshVaibhaw/wushang/commit/48fbd7e69e47ecf127eae376f698f8f9a6d980cf) | 2026-11-03 | Patch-distinct | Start the macOS port: dependencies and the porting contract |
