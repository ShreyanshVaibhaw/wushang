# Porting wushang to macOS

This branch (`macos`) turns the Windows player into a macOS app with the same features, interface and behaviour.
The Windows original is the reference: when in doubt, do what it does, pixel for pixel and feature for feature.
A pristine copy of it lives at `/Users/shreyanshvaibhaw/Dev/Wushang/src-windows` (read-only, never edit it).

Target: macOS 13+ on Apple Silicon (arm64), built with the Homebrew Rust toolchain, `cargo build`.
FFmpeg 9.0.2 and libass 0.17.5 come from Homebrew (`pkg-config` finds them; `.cargo/config.toml` sets `LIBCLANG_PATH`).
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
| Media Foundation encoders | `h264_videotoolbox`, `aac_at`, `libmp3lame` from Homebrew FFmpeg |
| DirectShow / GDI capture | FFmpeg `avfoundation` input |
| Registry file associations | `Info.plist` document types in the `.app` bundle |
| `%APPDATA%\wushang` | `~/Library/Application Support/wushang` (`WUSHANG_HOME` still overrides) |
| Videos / Music / Pictures / Desktop / Downloads | `~/Movies`, `~/Music`, `~/Pictures`, `~/Desktop`, `~/Downloads` |
| Ctrl shortcuts | Cmd (Ctrl also accepted); labels show ⌘, ⌥, ⇧ |
| Show in File Explorer | Show in Finder |
| Borderless fullscreen | native `toggleFullScreen:` |

## Module ownership during the port

Each agent edits only its own files.
Errors from other people's files are expected while the port runs; filter `cargo check` output to your own paths.
Anyone may add a dependency to `Cargo.toml`; add only lines, never reorder or remove others'.
Nobody commits; the coordinator does.

- **gfx**: `src/d2d/**` (new), `src/ui.rs`, `src/ui/**`, `src/captions.rs`.
- **video**: `src/render.rs`, `src/tonemap.rs`, `src/hdr.rs`, `src/nv12.rs`, `src/snapshot.rs`, `src/player.rs`, `src/shaders/**` (new).
- **services**: `src/{ass,assoc,audio,bookmarks,capture,codepage,convert,disc,dsp,library,net,prefs,probe,profiles,queue,record,session,stream,subs,tags,thumbs,update}.rs`, `build.rs`, `scripts/**`, `assets/macos/**` (new: Info.plist, icns).
- **shell**: `src/main.rs`, `src/platform/**` (new).

## Contracts between modules

These are fixed so the four parts can be written at the same time.
If you must change one, keep the old signature working or note the change at the bottom of this file under "Contract changes".

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

## Contract changes

(Append here: who, what, why.)
