# wushang 1.0.5 for Windows

Download the MSI to install or update wushang for your Windows account.
Use the ZIP to run it without installing.

- Play up to eight files in tabs, with independent queues, playback positions, tracks, volume and effects.
  Only the selected tab is audible; background tabs keep playing.
- Open a tab with Ctrl+T, close it with Ctrl+W, and switch with Ctrl+1 through Ctrl+9 or Ctrl+Tab.
  Files and streams also have a Play in New Tab menu item.
- Video keeps playing while the native file picker is open.
- Library scanning and saved-state writes run off the UI thread.
  Playlist edits preserve existing text bytes, and playlist renaming protects other files.
- Improve subtitle encoding detection, including UTF-16 files, and isolate attached fonts between playback sessions.
- Smooth volume changes, preserve preamp settings, and keep audio delay from shifting seek and resume positions.
- Keep toolbar controls within the available width and make omitted controls available in Customize.
- Remove incomplete recording and snapshot files after a failure, and handle malformed bookmarks and disc metadata more safely.
- Include the stream-playlist, feed-parser and subtitle-memory security fixes already merged into Windows main.

Validation passed 235 automated tests and release-app checks for playback, seeking, tabs, settings, mini-player, fullscreen and file dialogs.
One test that plays sound through the default device remains ignored.

This is a Windows release.
The macOS downloads remain available in release 1.0.4.
Explorer forwarding into an existing instance and the remaining macOS parity work are unfinished.
The bundled codec DLLs are unchanged; additional WMV/WMA decoder support in the build script is not included in these binaries.
