# Porting wushang to macOS

This branch (`macos`) is the Windows player as a macOS app with the same features, interface and behaviour.
The Windows app on `main` is the reference: when in doubt, do what it does, pixel for pixel and feature for feature.

Target: macOS 13+ on Apple Silicon (arm64), built with the Homebrew Rust toolchain, `cargo build`.
The app ships FFmpeg 9.0.2 and libass 0.17.5 as slim LGPL builds for macOS 13, made from source by `scripts/build-ffmpeg-macos.sh` and `scripts/build-libass-macos.sh` into `../tools` beside the repository.
`cargo build`, `cargo run` and `cargo test` use Homebrew's (`pkg-config` finds FFmpeg; `.cargo/config.toml` sets `LIBCLANG_PATH`), unless `FFMPEG_DIR` points them at the slim one.
There is no full Xcode, only the Command Line Tools: no `metal` shader compiler, no xcodebuild projects.
Metal shaders are compiled at run time from source with `newLibraryWithSource`.

## Mapping

| Windows | macOS |
|---|---|
| Win32 window, message loop | AppKit `NSWindow` + custom `NSView`, a manual event pump (`nextEventMatchingMask`) in the same shape as `pump_messages` |
| Direct3D 11 swap chain, video processor | `CAMetalLayer` + Metal shaders (YUV to RGB, flips, rotation, crop, zoom, colour adjust, tone mapping) |
| D3D11VA decoding | FFmpeg VideoToolbox hwaccel; frames are `CVPixelBuffer`s shown through `CVMetalTextureCache` with no copy |
| Direct2D + DirectWrite interface | `src/d2d/`: a shim with the same type and method names, backed by Core Graphics and Core Text |
| Direct2D SVG | resvg, rasterised per scale and cached |
| WIC (snapshots, pictures) | `image` crate |
| WASAPI via cpal | cpal on Core Audio (unchanged code) |
| WinHTTP | `ureq` |
| Media Foundation encoders | `h264_videotoolbox`, `aac_at`, `libmp3lame` (LAME linked into the slim FFmpeg) |
| DirectShow / GDI capture | FFmpeg `avfoundation` input |
| Registry file associations | `Info.plist` document types in the `.app` bundle |
| `%APPDATA%\wushang` | `~/Library/Application Support/wushang` (`WUSHANG_HOME` still overrides) |
| Videos / Music / Pictures / Desktop / Downloads | `~/Movies`, `~/Music`, `~/Pictures`, `~/Desktop`, `~/Downloads` |
| Ctrl shortcuts | Cmd (Ctrl also accepted); labels show ⌘, ⌥, ⇧ |
| Show in File Explorer | Show in Finder |
| Borderless fullscreen | native `toggleFullScreen:` |

## Carrying Windows changes over

`main` is the Windows app and `macos` this port, so `git merge main` (or a cherry-pick) brings upstream work across.
Most files merge as they are; these are where the two differ:

- `src/ui.rs`, `src/ui/**` and `src/captions.rs` differ from Windows mostly in their `use` lines, because `crate::d2d` (`src/d2d/`) has the same names and call shapes as windows-rs's Direct2D and DirectWrite.
  New drawing code from Windows usually compiles unchanged; a Direct2D or DirectWrite call the shim doesn't have yet goes into `src/d2d/`.
  After a merge, `cargo test render_check -- --ignored` paints the main screens into PNGs to look over.
- `src/main.rs` keeps the Windows `App` logic; the window, input and pickers are in `src/platform/` instead of Win32 calls.
- `src/render.rs`, `src/tonemap.rs`, `src/hdr.rs` and `src/player.rs`'s hardware setup are Metal and VideoToolbox code of their own; port changes there by hand.
- `src/net.rs`, `src/update.rs`, `src/assoc.rs`, `src/capture.rs`, `src/disc.rs`, `src/convert.rs`, `src/codepage.rs` and `src/ass.rs` swap Windows APIs for macOS ones (see the table above and the notes below).
- New FFmpeg components the Windows `scripts/build-ffmpeg.sh` gains go into `scripts/build-ffmpeg-macos.sh` too.

## Contracts between modules

How the parts of the macOS code meet.

### d2d shim (gfx provides, video and shell use)

`crate::d2d` re-exports names shaped like windows-rs's Direct2D/DirectWrite (`ID2D1DeviceContext5`, `ID2D1RenderTarget`, `ID2D1SolidColorBrush`, `IDWriteTextFormat`, `D2D_RECT_F`, `D2D1_COLOR_F`, `Matrix3x2`, ...), so `src/ui/**` changes little more than its `use` lines.
`crate::d2d::Result<T>` and `crate::d2d::Error` replace `windows::core::{Result, Error}`.
Entry points for the renderer:

```rust
impl ID2D1Factory1 { pub fn new() -> Self }
impl ID2D1DeviceContext5 {
    pub fn new() -> Self;
    /// Starts drawing into `cg`, a premultiplied BGRA bitmap context of `size` pixels, with one
    /// DIP being `scale` pixels (the window's backingScaleFactor). The shim sets D2D's top-left
    /// origin itself. It clears nothing.
    pub fn begin(&self, cg: &objc2_core_graphics::CGContext, size: (u32, u32), scale: f32);
    pub fn end(&self);
}
```

`ui::Painter::new(&ID2D1DeviceContext5, ID2D1Factory1) -> d2d::Result<Painter>` and `Painter::paint(&mut self, &ID2D1DeviceContext5, &Scene)` keep their Windows shapes.

### Renderer (video provides, shell uses)

`src/render.rs` keeps the Windows public API, with these Windows types replaced:

```rust
pub type Device = objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice>>;
pub fn create_device() -> Result<Device, Box<dyn std::error::Error>>;
impl Renderer {
    pub fn new(layer: Retained<objc2_quartz_core::CAMetalLayer>, size: (u32, u32), device: Device, lock: Option<DeviceLock>) -> Result<Self, ...>;
    pub fn resize(&mut self, width: u32, height: u32) -> Result<...>;          // pixels
    pub fn update_display(&mut self, window: &objc2_app_kit::NSWindow, allowed: bool, force: bool);
    pub fn present(&mut self, frame: Option<&ff::frame::Video>, scene: &Scene, dpi: f32, adjust: &VideoAdjust) -> Result<...>;
    // library_layout, add_overlay, add_dialog, add_thumbnail, set_seek_still, take_wanted, snapshot: unchanged
}
pub struct DeviceLock; // a no-op on Metal; DeviceLock::from_hw(&HwDevice) -> Option<Self>
```

`dpi` is still `96 * backingScaleFactor`, so the interface keeps working in DIPs.
The layer is created by the shell (the window's content view is layer-backed with a `CAMetalLayer`); the renderer sets its device, pixel format, colour space and drawable size.
`present` draws video into the rounded video area, the interface over it (through the d2d shim into an IOSurface-backed bitmap shown as a texture), and presents.

### Decoding (video provides)

```rust
impl HwDevice {
    pub fn videotoolbox() -> Option<Self>;   // replaces from_d3d11
}
```
`HwDevice` stays `Clone + Send`, and `open_video_decoder(..., Option<&HwDevice>)` keeps its shape.
Hardware frames reach the renderer as `AV_PIX_FMT_VIDEOTOOLBOX` (`data[3]` is the `CVPixelBufferRef`).

### Platform services (shell provides in `crate::platform`, anyone may call)

```rust
pub fn reveal_in_finder(path: &Path);
pub fn open_with_default_app(path_or_url: &Path);   // NSWorkspace openURL
pub fn keep_awake(on: bool);                         // IOPMAssertion while a video plays
pub fn system_dark_mode() -> bool;                   // NSApp effectiveAppearance; main thread
```

### Folders (services provides in `crate::library`)

`Library::default_dir()` returns `~/Library/Application Support/wushang` unless `WUSHANG_HOME` is set.
`snapshot::known_folder` and the first-run seeding use the macOS folders from the table above.

## Notes by module

- **shell**: keys, cursors and the clipboard for `src/ui/**`. The UI takes gfx's `d2d::VIRTUAL_KEY` (below); the shell's key mapping (`crate::platform::keys`) produces it, passing Cmd or Ctrl as `ctrl`.
  - `crate::platform::keys::shift_down() -> bool` replaces `GetKeyState(VK_SHIFT) < 0` (in `dialog_down`).
  - Typed text still arrives as `dialog_char(c: u32)` / `preferences_char(c: u32)`, one call per character after the key's `*_key` call, with Backspace also sent as 8, Enter 13, Esc 27, the way WM_CHAR sends them. Text comes through `interpretKeyEvents`, so dead keys and input methods work; Cmd/Ctrl shortcuts type nothing.
  - `Ui::dialog_cursor()` may keep returning `Option<d2d::PCWSTR>`; the shell maps `IDC_IBEAM` / `IDC_HAND` to `crate::platform::Cursor`.
  - `crate::platform::clipboard_text() -> Option<String>` and `crate::platform::set_clipboard_text(&str) -> bool` (NSPasteboard) replace dialog.rs's own clipboard functions.
  - `crate::platform::system_dark_mode()` is there for `ui::theme::system_light()`; the shell re-applies the theme when the system appearance changes.
  - Also in `crate::platform`: `reveal_in_finder`, `open_with_default_app`, `edit_text_file(&Path)` (the user's text editor), `play_sound(Vec<u8>)` (a WAV in memory), `keep_awake(bool)`.

- **gfx**: `crate::d2d` also carries the few non-graphics Win32 names the UI files use, so they keep their Windows shapes:
  `d2d::VIRTUAL_KEY(pub u16)` with the Windows `VK_*` values (`VK_ESCAPE`, `VK_RETURN`, `VK_LEFT`, `VK_A`, ...), which `Ui::dialog_key`, `preferences_key`, `customize_key` and `onboarding_key` take.
  The shell translates NSEvent key codes into these (Return, Escape, arrows, Home/End, Backspace = `VK_BACK`, forward Delete = `VK_DELETE`, letters = `VK_A`..`VK_Z`) and passes Cmd as `ctrl`.
  `Ui::dialog_cursor()` still returns `Option<d2d::PCWSTR>`; compare it with `d2d::IDC_IBEAM` / `d2d::IDC_HAND` and set `NSCursor.IBeamCursor` / `pointingHandCursor`.
  `d2d::Vector2`, `d2d::Matrix3x2` replace `windows_numerics`.
- **gfx**: `ui::theme::system_light()` calls `crate::platform::system_dark_mode()` (from the contract above) and negates it.
- **video**: renderer, decoding and snapshots.
  - `snapshot::Folder { Pictures, Desktop, Videos, Music, Downloads }` replaces the `FOLDERID_*` GUIDs, by value: `known_folder(Folder) -> std::io::Result<PathBuf>` (~/Pictures, ~/Desktop, ~/Movies, ~/Music, ~/Downloads) and `output_dir(key, Folder, &Path) -> std::io::Result<PathBuf>`. Errors print with `{e}`, not `e.message()`.
  - `Renderer::update_display(&NSWindow, allowed, force)` reads the screen's EDR headroom and the window's occlusion on every call (a few property reads), so call it every pass; `force` adds nothing. While the window is occluded `present` returns at once without drawing, so a hidden window never waits on `nextDrawable`.
  - The shell sets the layer's `contentsScale` (backingScaleFactor) and frame; the renderer sets device, pixel format (BGR10A2Unorm, or RGBA16Float in EDR), colour space (sRGB, or extended linear Display P3), `wantsExtendedDynamicRangeContent`, vsync, three drawables and `drawableSize` (from `new` and `resize`, in pixels).
  - EDR switches on only while an HDR10 or HLG frame is shown with HDR "Auto" on a screen whose potential headroom is above 1; the interface stays at SDR white. SDR video and the library never ask the screen for headroom.
  - `DeviceLock::from_hw(&HwDevice)` returns None: nothing needs locking on Metal. `HwDevice` is `Send`.
  - `render::takes(&frame) -> bool` says whether a VideoToolbox frame is shown straight from its pixel buffer; the decoder thread copies the others (one-plane formats such as ProRes 4444) back and repacks them.
  - Never `.clone()` a decoded frame: ffmpeg-next's `Clone` copies pixel data, which drops a VideoToolbox frame's pixel buffer. Move frames instead.
  - The interface bitmap is a Metal buffer's memory (shared on Apple silicon) wrapped in a `CGBitmapContext`, rather than an IOSurface: also no copy, and no IOSurface locking. `d2d.begin(&cg, size, scale)` gets a bitmap cleared to transparent each frame.
- **gfx**: `ID2D1DeviceContext5::begin(cg, size, scale)` saves the context's state, sets a top-left, y-down CTM in DIPs (`translate(0, h) * scale(scale, -scale)`), grayscale text, and takes the bitmap's colour space for every colour and image (so bytes come out as Direct2D's would, unconverted). It resets the world transform and clears nothing. `end()` pops any clip or layer still pushed, restores the context, and drops DrawText layouts not drawn for 3 frames: call it once per frame.
  `BeginDraw`/`EndDraw`/`Clear`/`SetDpi`/`GetSize` exist for code that still calls them; `EndDraw` does nothing (`end` finishes the frame).
  The shim's objects use `Rc`, so `Painter` and the context stay on one thread, as COM's did.
  `d2d::Canvas::new(w, h)` makes a premultiplied BGRA sRGB bitmap context (with `pixels()` and `save_png()`), for tests and tools.
  `cargo test render_check -- --ignored` paints the player, a menu, Settings, About, the library, the welcome, Customize Player and the settings popup into PNGs in `$WUSHANG_RENDER_DIR` (or the temp directory).
- **services**: folders, updates, capture, discs, converting.
  - `Library::default_dir()` is `~/Library/Application Support/wushang` (`WUSHANG_HOME` overrides); first run seeds ~/Movies and ~/Music. Hidden files are dot files and those with Finder's hidden flag (`UF_HIDDEN`).
  - `update::Release` keeps its fields: `zip` is the download (the release's `-macos-arm64.dmg`, else its `.zip`), `msi` is always None. `update::download` saves into ~/Downloads (through `snapshot::known_folder(Folder::Downloads)`) and selects the file in Finder itself (`open -R`); main only shows `Dialog::Downloaded`. `Saved::Installer` is never made; `install_on_exit`, `finish` and `clean_up` do nothing.
  - `assoc::register()` registers the running `.app` with Launch Services (`LSRegisterURL`) and does nothing for a bare exe. The document types are `assets/macos/Info.plist`'s (a test keeps them in step with `VIDEO_EXTS`/`AUDIO_EXTS`). `assoc::read` is gone.
  - Capture addresses are `avfoundation://video=<name>`, `avfoundation://audio=<name>` and `screen://` (`screen://1`, ... for more displays); `capture::devices()`, `open()` and `is_capture_path()` keep their shapes. Cameras open at their biggest mode up to 1080p that runs at 30 fps, in NV12 when they have it.
  - `disc::drives()` lists mounted volumes in /Volumes that hold a disc (UDF, ISO 9660, CD audio) or a VIDEO_TS/BDMV folder at the top; an empty drive shows nothing. Blu-ray no longer needs libbluray (the macOS FFmpeg has none): `disc::url` plays the longest BDMV/PLAYLIST/*.mpls's clips through `concat:`, and `disc::dvd_title` now also gives a Blu-ray's length and chapters. A `.iso` is mounted read-only with `hdiutil` under the temp folder (not in Finder) and detached when wushang exits.
  - `convert::Target::extension()` is "m4a" for `Mp3` when FFmpeg has no libmp3lame (AAC goes in M4A); name output files with it, as main does. Encoders: `h264_videotoolbox` then `libx264`, `aac_at` then `aac`, `libmp3lame`.
  - `prefs::FONTS` swaps Segoe UI and Consolas for Helvetica Neue and Menlo. `SNAP_DIR`/`RECORD_DIR` keys are unchanged (`pictures`, `videos`); their labels read Pictures/wushang and Movies/wushang.
  - `session::open_file` also leaves out the rtp, concat, avfoundation and lavfi demuxers, which Homebrew's FFmpeg has and the Windows build's slim FFmpeg doesn't.
  - `tags::Stamp`'s time is nanoseconds since 1970 (was FILETIME), so a tags.txt from Windows is probed again.
  - Packaging: `scripts/bundle-macos.sh` builds `<target>/bundle/wushang.app`, `wushang-<version>-macos-arm64.dmg` and `.zip`, with FFmpeg and libass in Contents/Frameworks; `build.rs` links bins with `-headerpad_max_install_names` and an rpath to `@executable_path/../Frameworks`. `scripts/make-testdata.sh` makes the test clips with Homebrew's ffmpeg (the minute-long ones now with a keyframe every 2 s, which record.rs's streaming test needs).
    Resolved: the bundle used to carry Homebrew's FFmpeg and libass, which are built for the Mac's own macOS (26 here), and Homebrew's FFmpeg is GPL (`--enable-gpl`, x264, x265).
    Both now come from source, built for macOS 13 (`MACOSX_DEPLOYMENT_TARGET=13.0`), as the Windows build's do:
    - `scripts/build-ffmpeg-macos.sh` builds FFmpeg 9.0.2, LGPL v2.1 or later, into `../tools/ffmpeg-9.0.2-wushang`, with the Windows build's protocols, demuxers, parsers, decoders, muxers and bitstream filters.
      macOS's own pieces replace Windows': the VideoToolbox hwaccels (H.264, HEVC, AV1, VP9, MPEG-1/2, ProRes), the `h264_videotoolbox`, `hevc_videotoolbox` and `aac_at` encoders, the `avfoundation` input device and Secure Transport for https.
      dav1d and LAME are linked in statically, zlib is macOS's, and FFmpeg's own `aac` encoder and the `ipod` (M4A) muxer are there for Convert's fallbacks.
      It also plays Monkey's Audio and AMR sound, which the Windows build can't, as `.ape` and `.3gp` are in wushang's file lists; ProRes and the PCM layouts Core Audio devices deliver are there too.
      There is no libbluray (nothing reaches FFmpeg's bluray protocol) and no libavfilter.
    - `scripts/build-libass-macos.sh` builds libass 0.17.5 into `../tools/libass-0.17.5`: one `libass.9.dylib` with FreeType 2.15.0, FriBidi 1.0.18 and HarfBuzz 15.0.0 inside, fonts through Core Text, exporting only libass's API.
    - Both download their pinned tarballs, check them against SHA-256 sums (of tarballs whose GPG signatures were checked), and fail unless every dylib is for macOS 13.0, is named `@rpath/lib*.dylib` and needs only system libraries.
    - `scripts/bundle-macos.sh` builds the release with `FFMPEG_DIR` set to the slim FFmpeg (ffmpeg-sys-next then links it from there instead of asking `pkg-config`, which finds Homebrew's) and `MACOSX_DEPLOYMENT_TARGET=13.0`, so a plain `scripts/bundle-macos.sh` needs nothing set.
      To develop against the slim FFmpeg too, `.cargo/config.toml` can set `FFMPEG_DIR` and `MACOSX_DEPLOYMENT_TARGET` in `[env]`, plus `rustflags = ['-C', 'link-arg=-Wl,-rpath,<FFmpeg folder>/lib']` under `[target.aarch64-apple-darwin]` so `cargo run` and `cargo test` find its `@rpath` dylibs; the whole test suite passes that way.
      The bundle script copies from `FFMPEG_DIR` and `LIBASS_DIR` (by default the folders above) and stops at any other library, so an exe linked against Homebrew's FFmpeg can't end up in the app.
      It takes that rpath out of the app's exe, and stops when any part of the app needs a newer macOS than Info.plist says, by its build version or by a strong import.
    - There is no `libx264`, so Convert's H.264 is `h264_videotoolbox` alone (with `allow_sw` it has a software encoder of its own).
    - `src/ass.rs` looks for libass in the app's Frameworks, then Homebrew's lib folders, so outside the app (`cargo run`, `cargo test`) it uses Homebrew's libass when there is one.
- **shell**: `Prefs::window` now holds the window's frame in points (left, top, width, height) measured from the main screen's top-left corner, y down, as AppKit sizes go; the "screen pixels" comment in `prefs.rs` (services) should say so. A Windows `settings.txt` copied over is just ignored when the spot is off every screen.
- **shell**: debug builds take scripted input for testing without the real mouse and keyboard: `WUSHANG_REMOTE=<fifo>`, one command per line (`key 49`, `click 300 20 2`, `move`, `drag`, `scroll`, `frame`, `menu`, `open`, `drop`, `cancel`, `track`, `appearance`, `terminate`); see `src/platform/remote.rs`. Not compiled into release builds.
