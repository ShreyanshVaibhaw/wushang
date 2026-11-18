# Windows release test, 2026-11-18

The optimized Windows executable is `D:/cargo-target/release/wushang.exe`.
The build uses the installed clang compiler for FFmpeg's C configuration probe.
Build output remains on D:.

The full automated run passed 235 tests, failed none, and ignored one test that plays sound on the default output device.
The long DSP pitch-and-duration test ran and passed.
The first full run found an outdated menu geometry assertion after adding New Tab and Close Tab.
The assertion now checks fourteen rows and the divider's updated location, preserving the original row dimensions.
The full suite was rerun successfully after that correction.

The release executable was launched with ExtraCursor and isolated settings in `D:/wushang-test/release-smoke-20261118`.
The sample videos were Alpha.mp4 and Beta.mp4 in `D:/wushang-test/tabs-media`.
The following checks passed:

- Playback rendered through D3D11VA, and pause and seek controls responded.
- Playback preferences opened and the speed slider changed from 1x to 1.5x.
- Ctrl+M entered the mini-player and restored the full player.
- F entered fullscreen and Escape restored the window.
- Ctrl+T opened the native picker and loaded Beta beside Alpha.
- Ctrl+1 and Ctrl+2 switched tabs; Alpha retained its paused position while Beta continued playing.
- Ctrl+W closed Beta and returned to Alpha.
- The main menu displayed New Tab and Close Tab without clipping.
- Escape returned to the library with its mini-player card.
- The test instance closed cleanly with exit code zero.

The logs reported normal decoder and audio-device initialization and no application failure during the UI checks.
The final release rebuild follows the test-only assertion correction.
Build and test logs are in `D:/wushang-tabs-port/release-build-final.log` and `D:/wushang-tabs-port/full-test-run-final.log`.

This validates the current port, not completion of every macOS-to-Windows parity task.
Explorer forwarding, codec DLL rebuilding and remaining shared fixes are still outstanding.

## Windows 1.0.5 packaging

The release version is 1.0.5.
`scripts/package.ps1` built the MSI and portable ZIP successfully, with user paths remapped out of the executable.
WiX validation passed, and decompiling the MSI confirmed version 1.0.5, per-user installation and the existing upgrade code.
The ZIP contains 18 files, each byte-identical to the staging folder, including the executable, DLLs and licenses.
The extracted executable launched with isolated settings, rendered video using D3D11VA, paused successfully and displayed Version 1.0.5 in About.
The packaged test instance closed with exit code zero.
The installer was validated without installing or upgrading the user's existing copy.
The codec build script passed Bash syntax checking and a configuration assertion check covering both present and missing decoders.

Package SHA-256 values:

- MSI: `b2114ada39589587585334733908a9728300c33bca401cd2b1040e56f9d337ca`
- ZIP: `a602d1da7b99f4620b910fb373e0bc60a43b4c23bfc50ed0687b9721fc07d41f`

The packaging log is `D:/wushang-tabs-port/package-1.0.5.log`.
