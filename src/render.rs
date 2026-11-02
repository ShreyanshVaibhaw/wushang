//! Metal presenter. VideoToolbox frames are CVPixelBuffers on IOSurfaces, which CVMetalTextureCache
//! hands to the shader as two textures (luma and chroma) without a copy. Software frames are
//! repacked to NV12 or P010 on the decoder thread (see `nv12`) and copied into textures of the same
//! layout, so both paths meet in one shader (shaders/video.metal): YCbCr to RGB with the frame's
//! matrix and range, the colour adjustments, and for BT.2020 the tone map of tonemap.rs. Crop,
//! aspect ratio, zoom, rotation and mirroring are all in where the quad's corners read the frame.
//!
//! The interface is painted by Core Graphics (through `crate::d2d`) into a premultiplied BGRA
//! bitmap whose memory is also a Metal texture, and goes over the video in the same pass.
//!
//! On a screen that can show EDR, HDR10 and HLG switch the layer to extended linear Display P3 in
//! half floats: the video keeps its nits up to most of the screen's headroom, and the interface
//! stays at SDR white.

use std::error::Error;
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;

use ff::ffi;
use ff::format::Pixel;
use ffmpeg_next as ff;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSWindow, NSWindowOcclusionState};
use objc2_core_foundation::{CFRetained, CFString, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceExtendedLinearDisplayP3, kCGColorSpaceSRGB,
};
use objc2_core_video::{
    CVBuffer, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane,
    kCVReturnSuccess,
};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin,
    MTLPixelFormat, MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResource, MTLResourceOptions,
    MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerState, MTLSize,
    MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::d2d::{ID2D1DeviceContext5, ID2D1Factory1};
use crate::hdr::Screen;
use crate::tonemap::{self, Signal};
use crate::ui::{Browse, Layout, Painter, Rect, Scene};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub type Device = Retained<ProtocolObject<dyn MTLDevice>>;
type Texture = Retained<ProtocolObject<dyn MTLTexture>>;
type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;
type Pipeline = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;

const SHADERS: &str = include_str!("shaders/video.metal");
/// Frames the CPU prepares while the GPU may still read an earlier one. The GPU finishes a frame
/// within a millisecond or two of its commit, so the second slot is free again long before the
/// next vsync, and each slot holds a full-window interface bitmap.
const FLIGHTS: usize = 2;
/// The largest texture Apple's GPUs take; a frame bigger than this is skipped.
const MAX_TEXTURE: u32 = 16384;

/// Metal needs no lock around the decoder: VideoToolbox and the renderer share only finished
/// IOSurfaces. Kept so the shell's code keeps the shape it has on Windows.
pub struct DeviceLock;

impl DeviceLock {
    pub fn from_hw(_hw: &crate::player::HwDevice) -> Option<Self> {
        None
    }
}

/// The GPU for decoding, video and the interface.
pub fn create_device() -> Result<Device> {
    Ok(MTLCreateSystemDefaultDevice().ok_or("no Metal device")?)
}

/// Mirroring of the picture as shown, so after rotation (the order Windows' video processor
/// applied them in): Horizontal always swaps the screen's left and right.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Flip {
    #[default]
    None,
    Horizontal,
    Vertical,
}

/// How the picture fills the video area.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Zoom {
    /// Fit inside the area, letterboxed.
    #[default]
    Original,
    /// Cover the area; the edges that overflow are cut off.
    Fill,
    /// This many times the fitted size, centred and cut to the area.
    Scale(f32),
}

/// VLC's video adjustments, applied by the shader on both decoding paths.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VideoAdjust {
    pub flip: Flip,
    /// Clockwise degrees: 0, 90, 180 or 270. Anything else counts as 0.
    pub rotate: u16,
    pub zoom: Zoom,
    /// Display aspect ratio of the whole frame; None takes it from the stream's sample aspect.
    pub aspect: Option<(u32, u32)>,
    /// Keep only the centre of the picture, cut to this display aspect ratio.
    pub crop: Option<(u32, u32)>,
    /// -1..1 each, 0 leaves the picture as decoded.
    pub brightness: f32,
    pub contrast: f32,
    pub hue: f32,
    pub saturation: f32,
}

impl Default for VideoAdjust {
    fn default() -> Self {
        VideoAdjust {
            flip: Flip::None,
            rotate: 0,
            zoom: Zoom::Original,
            aspect: None,
            crop: None,
            brightness: 0.0,
            contrast: 0.0,
            hue: 0.0,
            saturation: 0.0,
        }
    }
}

/// A rectangle in whole pixels of the target, right and bottom exclusive.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Px {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

pub struct Renderer {
    layer: Retained<CAMetalLayer>,
    gpu: Gpu,
    // Fields drop in order: the painter's resources go before their device context.
    painter: Painter,
    d2d: ID2D1DeviceContext5,
    /// The colours the painter's resources were made in.
    theme: crate::ui::theme::Theme,
    size: (u32, u32),
    flights: [Flight; FLIGHTS],
    next: usize,
    /// The window's screen when it can show EDR and the HDR setting allows it.
    screen: Option<Screen>,
    /// Set while the layer is in extended linear Display P3.
    edr: bool,
    /// False while the window is hidden or covered, when the layer may hand out no drawables.
    visible: bool,
}

/// What one frame in flight uses. The present that takes the slot next waits for the GPU first.
#[derive(Default)]
struct Flight {
    done: Option<CommandBuffer>,
    canvas: Option<Canvas>,
    upload: Option<Upload>,
    /// The frame's textures, and the pixel buffer behind them, until the GPU has read them.
    planes: Option<Planes>,
}

impl Flight {
    fn wait(&mut self) {
        if let Some(done) = self.done.take() {
            done.waitUntilCompleted();
        }
        self.planes = None;
    }
}

impl Renderer {
    pub fn new(
        layer: Retained<CAMetalLayer>,
        size: (u32, u32),
        device: Device,
        _lock: Option<DeviceLock>,
    ) -> Result<Self> {
        let mut gpu = Gpu::new(device)?;
        // Both outputs' pipelines now, so the first frame and the first HDR one don't stall.
        gpu.pipelines(MTLPixelFormat::BGR10A2Unorm)?;
        gpu.pipelines(MTLPixelFormat::RGBA16Float)?;
        layer.setDevice(Some(&gpu.device));
        layer.setFramebufferOnly(true);
        // Vsync, without tying presents to Core Animation transactions, and at most a second's
        // wait for a drawable so a stalled compositor can't hang the window.
        layer.setDisplaySyncEnabled(true);
        layer.setPresentsWithTransaction(false);
        layer.setMaximumDrawableCount(3);
        layer.setAllowsNextDrawableTimeout(true);
        set_output(&layer, false);
        layer.setDrawableSize(CGSize::new(size.0 as f64, size.1 as f64));
        let d2d = ID2D1DeviceContext5::new();
        let painter = Painter::new(&d2d, ID2D1Factory1::new())?;
        Ok(Renderer {
            layer,
            gpu,
            painter,
            d2d,
            theme: crate::ui::theme::current(),
            size,
            flights: Default::default(),
            next: 0,
            screen: None,
            edr: false,
            visible: true,
        })
    }

    /// The drawable's new size in pixels.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 || (width, height) == self.size {
            return Ok(());
        }
        self.layer
            .setDrawableSize(CGSize::new(width as f64, height as f64));
        self.size = (width, height);
        Ok(())
    }

    /// Follows the window's screen and whether it is visible; `allowed` is the HDR setting. The
    /// screen's headroom moves with its brightness, so this reads it on every call, which costs
    /// a few property reads: call it every pass. `force` (a display change) adds nothing to that.
    pub fn update_display(&mut self, window: &NSWindow, allowed: bool, _force: bool) {
        self.visible = window
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible);
        self.screen = allowed.then(|| crate::hdr::screen(window)).flatten();
    }

    /// Switches the layer in or out of EDR. Only HDR10 and HLG frames need it, so the interface
    /// and SDR video never ask the screen for headroom.
    fn set_edr(&mut self, edr: bool) {
        if edr != self.edr {
            set_output(&self.layer, edr);
            self.edr = edr;
        }
    }

    pub fn library_layout(
        &self,
        width: f32,
        height: f32,
        browse: &Browse,
        card: Option<bool>,
    ) -> Layout {
        self.painter.library_layout(width, height, browse, card)
    }

    pub fn add_overlay(&self, layout: &mut Layout, browse: &Browse) {
        self.painter.add_overlay(layout, browse);
    }

    /// Adds the open dialog to a layout.
    pub fn add_dialog(&self, layout: &mut Layout, dialog: &crate::ui::dialog::State) {
        self.painter.add_dialog(layout, dialog);
    }

    pub fn add_thumbnail(&mut self, path: PathBuf, picture: Option<(u32, u32, &[u8])>) {
        self.painter.add_thumbnail(&self.d2d, path, picture);
    }

    pub fn set_seek_still(&mut self, still: Option<(u32, u32, &[u8])>) {
        self.painter.set_seek_still(&self.d2d, still);
    }

    pub fn take_wanted(&mut self) -> Vec<PathBuf> {
        self.painter.take_wanted()
    }

    /// Draws the video frame (if any) into the scene's video area, paints the interface around
    /// and over it, and presents at the next vsync. `dpi` converts the scene's DIPs to pixels.
    pub fn present(
        &mut self,
        frame: Option<&ff::frame::Video>,
        scene: &Scene,
        dpi: f32,
        adjust: &VideoAdjust,
    ) -> Result<()> {
        let theme = crate::ui::theme::current();
        if theme != self.theme {
            let mut fresh = Painter::new(&self.d2d, self.painter.factory())?;
            fresh.carry_over(&mut self.painter);
            self.painter = fresh;
            self.theme = theme;
        }
        if !self.visible {
            return Ok(());
        }
        // The drawable and Metal's other autoreleased objects go back every frame.
        autoreleasepool(|_| self.draw(frame, scene, dpi, adjust))
    }

    fn draw(
        &mut self,
        frame: Option<&ff::frame::Video>,
        scene: &Scene,
        dpi: f32,
        adjust: &VideoAdjust,
    ) -> Result<()> {
        let slot = self.next;
        self.next = (slot + 1) % FLIGHTS;
        let flight = &mut self.flights[slot];
        flight.wait();
        // Lets the cache recycle the wrappers of pixel buffers the GPU is done with.
        self.gpu.cache.flush(0);
        let area = to_pixels(scene.layout.video.scaled(dpi / 96.0));
        // A frame the GPU won't take (wider than its 16384 limit, say) is skipped, and the
        // interface still paints, so a crafted file can't freeze or close the window.
        let video = frame.and_then(|frame| {
            self.gpu
                .prepare(frame, adjust, area, &mut flight.upload)
                .unwrap_or_else(|e| {
                    static TOLD: std::sync::Once = std::sync::Once::new();
                    TOLD.call_once(|| eprintln!("video frame: {e}"));
                    None
                })
        });
        let screen = self
            .screen
            .filter(|_| video.as_ref().is_some_and(|v| v.colors.hdr()));
        self.set_edr(screen.is_some());
        let px = 96.0 / dpi;
        self.painter.picture = video.as_ref().map(|v| {
            let d = v.place.dest;
            Rect::new(d.left as f32, d.top as f32, d.right as f32, d.bottom as f32).scaled(px)
        });

        let flight = &mut self.flights[slot];
        if flight.canvas.as_ref().is_none_or(|c| c.size != self.size) {
            flight.canvas = Some(Canvas::new(&self.gpu.device, self.size)?);
        }
        let canvas = flight.canvas.as_ref().unwrap();
        canvas.clear();
        self.d2d.begin(&canvas.cg, self.size, dpi / 96.0);
        let painted = self.painter.paint(&self.d2d, scene);
        self.d2d.end();
        painted?;

        let Some(drawable) = self.layer.nextDrawable() else {
            return Ok(()); // timed out: this frame is dropped, the next pass tries again
        };
        let target = drawable.texture();
        let commands = self.gpu.queue.commandBuffer().ok_or("no command buffer")?;
        let (video_pipe, ui_pipe) = self.gpu.pipelines(target.pixelFormat())?;
        let encoder = begin_pass(&commands, &target)?;
        let size = (target.width() as u32, target.height() as u32);
        if let Some(v) = &video {
            self.gpu.encode(&encoder, &video_pipe, v, screen, size);
        }
        if canvas.size == size {
            encode_interface(&encoder, &ui_pipe, &canvas.texture, self.edr);
        }
        encoder.endEncoding();
        commands.presentDrawable(ProtocolObject::from_ref(&*drawable));
        commands.commit();
        flight.done = Some(commands);
        flight.planes = video.map(|v| v.planes);
        Ok(())
    }

    /// The frame as shown, adjusted but not zoomed, at its own resolution: width, height and
    /// BGRA rows. A sample aspect ratio other than 1 widens or narrows it, as on screen.
    pub fn snapshot(
        &mut self,
        frame: &ff::frame::Video,
        adjust: &VideoAdjust,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let adjust = VideoAdjust {
            zoom: Zoom::Original,
            ..*adjust
        };
        let (_, (pw, ph)) = shown(frame.width(), frame.height(), sample_aspect(frame), &adjust);
        let (w, h) = ((pw.round() as u32).max(1), (ph.round() as u32).max(1));
        let pixels = autoreleasepool(|_| {
            self.gpu
                .render(frame, &adjust, (w, h), MTLPixelFormat::BGRA8Unorm, None)
        })?;
        Ok((w, h, pixels))
    }
}

/// The layer's pixels: 10-bit sRGB, or half-float extended linear Display P3 for EDR, where 1.0
/// is SDR white and the screen shows more up to its headroom.
fn set_output(layer: &CAMetalLayer, edr: bool) {
    let (format, space): (_, &CFString) = unsafe {
        if edr {
            (
                MTLPixelFormat::RGBA16Float,
                kCGColorSpaceExtendedLinearDisplayP3,
            )
        } else {
            (MTLPixelFormat::BGR10A2Unorm, kCGColorSpaceSRGB)
        }
    };
    layer.setPixelFormat(format);
    layer.setColorspace(CGColorSpace::with_name(Some(space)).as_deref());
    layer.setWantsExtendedDynamicRangeContent(edr);
}

/// The Metal objects video drawing needs, for the window and for snapshots.
struct Gpu {
    device: Device,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    /// The video and interface pipelines for each target format seen.
    pipelines: Vec<(MTLPixelFormat, Pipeline, Pipeline)>,
    cache: CFRetained<CVMetalTextureCache>,
    /// For a software frame that reaches the renderer in another format than NV12 or P010.
    nv12: crate::nv12::Nv12,
}

impl Gpu {
    fn new(device: Device) -> Result<Self> {
        let queue = device.newCommandQueue().ok_or("no command queue")?;
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADERS), None)
            .map_err(|e| format!("shaders: {}", e.localizedDescription()))?;
        let desc = MTLSamplerDescriptor::new();
        desc.setMinFilter(MTLSamplerMinMagFilter::Linear);
        desc.setMagFilter(MTLSamplerMinMagFilter::Linear);
        desc.setSAddressMode(MTLSamplerAddressMode::ClampToEdge);
        desc.setTAddressMode(MTLSamplerAddressMode::ClampToEdge);
        let sampler = device
            .newSamplerStateWithDescriptor(&desc)
            .ok_or("no sampler")?;
        let mut cache = std::ptr::null_mut();
        let made = unsafe {
            CVMetalTextureCache::create(None, None, &device, None, NonNull::from(&mut cache))
        };
        let cache = NonNull::new(cache)
            .filter(|_| made == kCVReturnSuccess)
            .ok_or("no Metal texture cache")?;
        Ok(Gpu {
            cache: unsafe { CFRetained::from_raw(cache) },
            device,
            queue,
            library,
            sampler,
            pipelines: Vec::new(),
            nv12: Default::default(),
        })
    }

    /// The video and interface pipelines for drawing into `format`.
    fn pipelines(&mut self, format: MTLPixelFormat) -> Result<(Pipeline, Pipeline)> {
        if let Some((_, video, ui)) = self.pipelines.iter().find(|p| p.0 == format) {
            return Ok((video.clone(), ui.clone()));
        }
        let make = |fragment: &str, blend: bool| -> Result<Pipeline> {
            let function = |name: &str| {
                self.library
                    .newFunctionWithName(&NSString::from_str(name))
                    .ok_or(format!("no shader {name}"))
            };
            let desc = MTLRenderPipelineDescriptor::new();
            desc.setVertexFunction(Some(&*function("corners")?));
            desc.setFragmentFunction(Some(&*function(fragment)?));
            let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
            color.setPixelFormat(format);
            if blend {
                // The interface is premultiplied: source over.
                use objc2_metal::MTLBlendFactor;
                color.setBlendingEnabled(true);
                color.setSourceRGBBlendFactor(MTLBlendFactor::One);
                color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
                color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
                color.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            }
            Ok(self
                .device
                .newRenderPipelineStateWithDescriptor_error(&desc)
                .map_err(|e| format!("pipeline: {}", e.localizedDescription()))?)
        };
        let video = make("video", false)?;
        let ui = make("interface", true)?;
        self.pipelines.push((format, video.clone(), ui.clone()));
        Ok((video, ui))
    }

    /// Everything `encode` needs to draw `frame` into `area`, or None when none of the picture
    /// is visible. `upload` holds the textures a software frame is copied into.
    fn prepare(
        &mut self,
        frame: &ff::frame::Video,
        adjust: &VideoAdjust,
        area: Px,
        upload: &mut Option<Upload>,
    ) -> Result<Option<Prepared>> {
        let (w, h) = (frame.width(), frame.height());
        if w > MAX_TEXTURE || h > MAX_TEXTURE {
            return Err(format!("{w}x{h} is larger than the GPU's {MAX_TEXTURE}").into());
        }
        let Some(place) = geometry(w, h, sample_aspect(frame), adjust, area) else {
            return Ok(None);
        };
        let (planes, colors) = if frame.format() == Pixel::VIDEOTOOLBOX {
            (self.wrap(frame)?, colors(frame))
        } else {
            let converted;
            let nv12 = if matches!(frame.format(), Pixel::NV12 | Pixel::P010LE) {
                frame
            } else {
                converted = self.nv12.convert(frame)?;
                &converted
            };
            (self.upload(nv12, upload)?, colors(nv12))
        };
        Ok(Some(Prepared {
            params: params(&colors, &planes, adjust),
            planes,
            colors,
            place,
        }))
    }

    /// A VideoToolbox frame's planes as textures on its own IOSurface.
    fn wrap(&self, frame: &ff::frame::Video) -> Result<Planes> {
        // VideoToolbox frames carry their CVPixelBufferRef in data[3].
        let buffer = unsafe { (*frame.as_ptr()).data[3] } as *mut CVBuffer;
        let buffer = NonNull::new(buffer).ok_or("frame has no pixel buffer")?;
        let pixels = unsafe { buffer.as_ref() };
        let format = CVPixelBufferGetPixelFormatType(pixels);
        let bits = cv_bits(format)
            .filter(|_| CVPixelBufferGetPlaneCount(pixels) == 2)
            .ok_or_else(|| format!("no shader for pixel buffers in {:?}", fourcc(format)))?;
        let formats = match bits {
            8 => [MTLPixelFormat::R8Unorm, MTLPixelFormat::RG8Unorm],
            _ => [MTLPixelFormat::R16Unorm, MTLPixelFormat::RG16Unorm],
        };
        let mut held = Vec::with_capacity(2);
        let mut textures = Vec::with_capacity(2);
        for (plane, format) in formats.into_iter().enumerate() {
            let (w, h) = (
                CVPixelBufferGetWidthOfPlane(pixels, plane),
                CVPixelBufferGetHeightOfPlane(pixels, plane),
            );
            let mut out = std::ptr::null_mut();
            let made = unsafe {
                CVMetalTextureCache::create_texture_from_image(
                    None,
                    &self.cache,
                    pixels,
                    None,
                    format,
                    w,
                    h,
                    plane,
                    NonNull::from(&mut out),
                )
            };
            let out = NonNull::new(out)
                .filter(|_| made == kCVReturnSuccess)
                .ok_or_else(|| format!("no texture for plane {plane} ({made})"))?;
            let out = unsafe { CFRetained::from_raw(out) };
            textures.push(CVMetalTextureGetTexture(&out).ok_or("no Metal texture")?);
            held.push(out);
        }
        let chroma = textures.pop().unwrap();
        let luma = textures.pop().unwrap();
        Ok(Planes {
            luma,
            chroma,
            bits,
            // Also keeps the buffer from going back to the decoder's pool while the GPU reads.
            _held: Some((unsafe { CFRetained::retain(buffer) }, held)),
        })
    }

    /// Copies an NV12 or P010 frame into `upload`'s textures, made or remade to fit.
    fn upload(&self, frame: &ff::frame::Video, upload: &mut Option<Upload>) -> Result<Planes> {
        let size = (frame.width(), frame.height());
        // P010 is NV12 with each sample in the top 10 bits of 16.
        let p010 = frame.format() == Pixel::P010LE;
        let chroma_size = (size.0.div_ceil(2), size.1.div_ceil(2));
        if upload
            .as_ref()
            .is_none_or(|u| (u.size, u.p010) != (size, p010))
        {
            let (luma, chroma) = if p010 {
                (MTLPixelFormat::R16Unorm, MTLPixelFormat::RG16Unorm)
            } else {
                (MTLPixelFormat::R8Unorm, MTLPixelFormat::RG8Unorm)
            };
            *upload = Some(Upload {
                size,
                p010,
                luma: self.texture(luma, size, MTLTextureUsage::ShaderRead)?,
                chroma: self.texture(chroma, chroma_size, MTLTextureUsage::ShaderRead)?,
            });
        }
        let u = upload.as_ref().unwrap();
        let sample = if p010 { 2 } else { 1 };
        for (plane, texture, (w, h), bytes) in [
            (0, &u.luma, size, sample),
            (1, &u.chroma, chroma_size, 2 * sample),
        ] {
            let data = frame.data(plane);
            let stride = frame.stride(plane);
            // A bottom row shorter than the stride ends the plane's data.
            if data.len() < stride * (h as usize - 1) + w as usize * bytes {
                return Err("frame plane is short".into());
            }
            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: w as usize,
                    height: h as usize,
                    depth: 1,
                },
            };
            let pixels = NonNull::new(data.as_ptr() as *mut c_void).ok_or("empty plane")?;
            unsafe {
                texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, pixels, stride)
            };
        }
        Ok(Planes {
            luma: u.luma.clone(),
            chroma: u.chroma.clone(),
            bits: if p010 { 10 } else { 8 },
            _held: None,
        })
    }

    /// A texture the CPU can write and read, in memory both share on Apple silicon.
    fn texture(
        &self,
        format: MTLPixelFormat,
        (w, h): (u32, u32),
        usage: MTLTextureUsage,
    ) -> Result<Texture> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                format, w as usize, h as usize, false,
            )
        };
        desc.setUsage(usage);
        desc.setStorageMode(if self.device.hasUnifiedMemory() {
            MTLStorageMode::Shared
        } else {
            MTLStorageMode::Managed
        });
        Ok(self
            .device
            .newTextureWithDescriptor(&desc)
            .ok_or("no texture")?)
    }

    /// Draws a prepared frame into a `size` target: for an EDR `screen`, or for sRGB.
    fn encode(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        pipeline: &Pipeline,
        v: &Prepared,
        screen: Option<Screen>,
        size: (u32, u32),
    ) {
        let (tw, th) = (v.planes.luma.width() as f64, v.planes.luma.height() as f64);
        let d = v.place.dest;
        let x = |x: i32| (2.0 * f64::from(x) / f64::from(size.0) - 1.0) as f32;
        let y = |y: i32| (1.0 - 2.0 * f64::from(y) / f64::from(size.1)) as f32;
        let position = [
            [x(d.left), y(d.top)],
            [x(d.right), y(d.top)],
            [x(d.left), y(d.bottom)],
            [x(d.right), y(d.bottom)],
        ];
        let corners: [Corner; 4] = std::array::from_fn(|i| Corner {
            position: position[i],
            uv: [
                (v.place.source[i][0] / tw) as f32,
                (v.place.source[i][1] / th) as f32,
            ],
        });
        let mut params = v.params;
        output(&mut params, &v.colors, screen);
        unsafe {
            encoder.setRenderPipelineState(pipeline);
            set_vertex_bytes(encoder, &corners);
            encoder.setFragmentTexture_atIndex(Some(&v.planes.luma), 0);
            encoder.setFragmentTexture_atIndex(Some(&v.planes.chroma), 1);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
            encoder.setFragmentBytes_length_atIndex(
                NonNull::from(&params).cast(),
                size_of::<Params>(),
                0,
            );
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        }
    }

    /// Draws `frame` alone into a fresh `size` texture of `format` and reads it back: tightly
    /// packed rows, black where the picture doesn't reach. For snapshots and the tests.
    fn render(
        &mut self,
        frame: &ff::frame::Video,
        adjust: &VideoAdjust,
        size: (u32, u32),
        format: MTLPixelFormat,
        screen: Option<Screen>,
    ) -> Result<Vec<u8>> {
        let target = self.texture(format, size, MTLTextureUsage::RenderTarget)?;
        let area = Px {
            left: 0,
            top: 0,
            right: size.0 as i32,
            bottom: size.1 as i32,
        };
        let video = self.prepare(frame, adjust, area, &mut None)?;
        let (pipeline, _) = self.pipelines(format)?;
        let commands = self.queue.commandBuffer().ok_or("no command buffer")?;
        let encoder = begin_pass(&commands, &target)?;
        if let Some(v) = &video {
            self.encode(&encoder, &pipeline, v, screen, size);
        }
        encoder.endEncoding();
        if target.storageMode() == MTLStorageMode::Managed {
            let blit = commands.blitCommandEncoder().ok_or("no blit encoder")?;
            blit.synchronizeResource(ProtocolObject::from_ref(&*target));
            blit.endEncoding();
        }
        commands.commit();
        commands.waitUntilCompleted();
        let pixel = match format {
            MTLPixelFormat::RGBA16Float => 8,
            _ => 4,
        };
        let row = size.0 as usize * pixel;
        let mut pixels = vec![0u8; row * size.1 as usize];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: size.0 as usize,
                height: size.1 as usize,
                depth: 1,
            },
        };
        unsafe {
            target.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new(pixels.as_mut_ptr()).unwrap().cast(),
                row,
                region,
                0,
            )
        };
        Ok(pixels)
    }
}

/// A render pass on `target`, cleared to black.
fn begin_pass(
    commands: &ProtocolObject<dyn MTLCommandBuffer>,
    target: &ProtocolObject<dyn MTLTexture>,
) -> Result<Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>> {
    let pass = MTLRenderPassDescriptor::renderPassDescriptor();
    let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
    color.setTexture(Some(target));
    color.setLoadAction(MTLLoadAction::Clear);
    color.setClearColor(MTLClearColor {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 1.0,
    });
    color.setStoreAction(MTLStoreAction::Store);
    Ok(commands
        .renderCommandEncoderWithDescriptor(&pass)
        .ok_or("no render encoder")?)
}

/// Blends the interface bitmap over the whole target, pixel for pixel.
fn encode_interface(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &Pipeline,
    texture: &ProtocolObject<dyn MTLTexture>,
    edr: bool,
) {
    let corners = [[-1.0, 1.0], [1.0, 1.0], [-1.0, -1.0], [1.0, -1.0]].map(|position| Corner {
        position,
        uv: [0.0; 2],
    });
    let params = Interface {
        gamut: rows(tonemap::BT709_TO_P3),
        mode: [i32::from(edr), 0, 0, 0],
    };
    unsafe {
        encoder.setRenderPipelineState(pipeline);
        set_vertex_bytes(encoder, &corners);
        encoder.setFragmentTexture_atIndex(Some(texture), 0);
        encoder.setFragmentBytes_length_atIndex(
            NonNull::from(&params).cast(),
            size_of::<Interface>(),
            0,
        );
        encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
    }
}

unsafe fn set_vertex_bytes(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    corners: &[Corner; 4],
) {
    unsafe {
        encoder.setVertexBytes_length_atIndex(
            NonNull::from(corners).cast(),
            size_of::<[Corner; 4]>(),
            0,
        )
    };
}

/// The interface's bitmap: Core Graphics draws into the buffer's memory, and the GPU reads the
/// same memory as a texture, so nothing is copied.
struct Canvas {
    size: (u32, u32),
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    texture: Texture,
    cg: CFRetained<CGContext>,
}

impl Canvas {
    fn new(device: &Device, size: (u32, u32)) -> Result<Self> {
        let (w, h) = (size.0 as usize, size.1 as usize);
        let format = MTLPixelFormat::BGRA8Unorm;
        let align = device
            .minimumLinearTextureAlignmentForPixelFormat(format)
            .max(16);
        let row = (w * 4).next_multiple_of(align);
        let buffer = device
            .newBufferWithLength_options(row * h, MTLResourceOptions::StorageModeShared)
            .ok_or("no interface buffer")?;
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                format, w, h, false,
            )
        };
        desc.setStorageMode(MTLStorageMode::Shared);
        desc.setUsage(MTLTextureUsage::ShaderRead);
        let texture = buffer
            .newTextureWithDescriptor_offset_bytesPerRow(&desc, 0, row)
            .ok_or("no interface texture")?;
        // Premultiplied BGRA in memory, the layout of Metal's BGRA8Unorm.
        let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        let cg = unsafe {
            CGBitmapContextCreate(
                buffer.contents().as_ptr(),
                w,
                h,
                8,
                row,
                space.as_deref(),
                info,
            )
        }
        .ok_or("no interface bitmap context")?;
        Ok(Canvas {
            size,
            buffer,
            texture,
            cg,
        })
    }

    /// Transparent, so the video shows wherever the interface paints nothing.
    fn clear(&self) {
        unsafe {
            std::ptr::write_bytes(
                self.buffer.contents().as_ptr().cast::<u8>(),
                0,
                self.buffer.length(),
            )
        };
    }
}

/// A frame's planes as textures, and what keeps a VideoToolbox frame's valid.
struct Planes {
    luma: Texture,
    chroma: Texture,
    /// Bits per sample: 8, or 10 at the top of 16.
    bits: u32,
    /// The pixel buffer and the texture cache's wrappers for its planes.
    _held: Option<(CFRetained<CVBuffer>, Vec<CFRetained<CVBuffer>>)>,
}

/// Textures a software frame is copied into, kept while frames keep their size and depth.
struct Upload {
    size: (u32, u32),
    p010: bool,
    luma: Texture,
    chroma: Texture,
}

/// A frame ready to draw.
struct Prepared {
    planes: Planes,
    colors: Colors,
    place: Placement,
    /// The shader's numbers that don't depend on the screen.
    params: Params,
}

/// One corner of a quad, as shaders/video.metal reads it.
#[repr(C)]
#[derive(Clone, Copy)]
struct Corner {
    position: [f32; 2],
    uv: [f32; 2],
}

/// shaders/video.metal's `Params`; see there for each member.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Params {
    scale: [f32; 4],
    offset: [f32; 4],
    chroma: [f32; 4],
    matrix: [[f32; 4]; 3],
    adjust: [f32; 4],
    hue: [f32; 4],
    gamut: [[f32; 4]; 3],
    tone: [f32; 4],
    mode: [i32; 4],
}

/// shaders/video.metal's `Interface`.
#[repr(C)]
struct Interface {
    gamut: [[f32; 4]; 3],
    mode: [i32; 4],
}

fn rows(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    m.map(|[a, b, c]| [a, b, c, 0.0])
}

/// The shader's numbers for a frame in `colors` read from `planes`, adjusted.
fn params(colors: &Colors, planes: &Planes, adjust: &VideoAdjust) -> Params {
    // A sample's code value at full scale: 255 for 8 bits, 65535 / 64 for 10 at the top of 16.
    let code = match planes.bits {
        8 => 255.0,
        _ => 65535.0 / 64.0,
    };
    let step = 2f32.powi(planes.bits as i32 - 8);
    let (luma, chroma) = if colors.full {
        let max = 2f32.powi(planes.bits as i32) - 1.0;
        ((code / max, 0.0), (code / max, 128.0 * step / max))
    } else {
        (
            (code / (219.0 * step), 16.0 / 219.0),
            (code / (224.0 * step), 128.0 / 224.0),
        )
    };
    let (kr, kb) = colors.coefficients;
    let kg = 1.0 - kr - kb;
    let matrix = [
        [1.0, 0.0, 2.0 * (1.0 - kr), 0.0],
        [
            1.0,
            -2.0 * (1.0 - kb) * kb / kg,
            -2.0 * (1.0 - kr) * kr / kg,
            0.0,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0, 0.0],
    ];
    // Chroma sits a quarter of its own sample to the left (or above) of where an evenly spread
    // texture puts it, for the common left and top-left sitings of 4:2:0.
    let subsampled = (
        planes.chroma.width() < planes.luma.width(),
        planes.chroma.height() < planes.luma.height(),
    );
    let shift = |on: bool, s: f32, size: usize| if on { s / size as f32 } else { 0.0 };
    let chroma_at = [
        shift(subsampled.0, colors.siting.0, planes.chroma.width()),
        shift(subsampled.1, colors.siting.1, planes.chroma.height()),
        0.0,
        0.0,
    ];
    let level = |l: f32| l.clamp(-1.0, 1.0);
    // Contrast and saturation are gains up to 2x, like VLC's adjust filter; brightness adds up to
    // 100 of 255 levels and hue turns up to half a circle either way, the D3D11 video
    // processor's ranges on Windows.
    let turn = level(adjust.hue) * std::f32::consts::PI;
    Params {
        scale: [luma.0, chroma.0, chroma.0, 0.0],
        offset: [luma.1, chroma.1, chroma.1, 0.0],
        chroma: chroma_at,
        matrix,
        adjust: [
            1.0 + level(adjust.contrast),
            level(adjust.brightness) * 100.0 / 255.0,
            1.0 + level(adjust.saturation),
            0.0,
        ],
        hue: [turn.cos(), turn.sin(), 0.0, 0.0],
        ..Default::default()
    }
}

/// Fills in the numbers that depend on where the frame goes: an EDR `screen`, or sRGB.
fn output(params: &mut Params, colors: &Colors, screen: Option<Screen>) {
    let edr = screen.is_some();
    let Some(signal) = colors.signal else {
        params.gamut = rows(tonemap::BT709_TO_P3);
        params.mode = [0, i32::from(edr), 0, 0];
        return;
    };
    let t = tonemap::tone(signal, colors.peak, screen);
    params.gamut = rows(if edr {
        tonemap::BT2020_TO_P3
    } else {
        tonemap::BT2020_TO_709
    });
    params.tone = [t.nits, t.peak, t.top, t.scale];
    params.mode = [signal.code(), i32::from(edr), 0, 0];
}

/// Bits per sample of the two-plane CVPixelBuffer formats the shader reads, None for others.
#[allow(non_upper_case_globals)]
fn cv_bits(format: u32) -> Option<u32> {
    use objc2_core_video::*;
    match format {
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        | kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        | kCVPixelFormatType_422YpCbCr8BiPlanarVideoRange
        | kCVPixelFormatType_422YpCbCr8BiPlanarFullRange
        | kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange
        | kCVPixelFormatType_444YpCbCr8BiPlanarFullRange => Some(8),
        kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
        | kCVPixelFormatType_420YpCbCr10BiPlanarFullRange
        | kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange
        | kCVPixelFormatType_422YpCbCr10BiPlanarFullRange
        | kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange
        | kCVPixelFormatType_444YpCbCr10BiPlanarFullRange => Some(10),
        _ => None,
    }
}

fn fourcc(format: u32) -> String {
    String::from_utf8_lossy(&format.to_be_bytes()).into_owned()
}

/// Whether the renderer shows this VideoToolbox frame straight from its pixel buffer; the
/// decoder thread copies the others back and repacks them.
pub fn takes(frame: &ff::frame::Video) -> bool {
    let buffer = unsafe { (*frame.as_ptr()).data[3] } as *const CVBuffer;
    let Some(pixels) = (unsafe { buffer.as_ref() }) else {
        return false;
    };
    cv_bits(CVPixelBufferGetPixelFormatType(pixels)).is_some()
        && CVPixelBufferGetPlaneCount(pixels) == 2
}

fn to_pixels(r: Rect) -> Px {
    Px {
        left: r.l.round() as i32,
        top: r.t.round() as i32,
        right: r.r.round() as i32,
        bottom: r.b.round() as i32,
    }
}

/// Untagged HD video is BT.709 by convention and SD is BT.601, the same guess VLC and mpv make.
pub fn is_bt709(frame: &ff::frame::Video) -> bool {
    match frame.color_space() {
        ff::color::Space::BT709 => true,
        ff::color::Space::Unspecified => frame.height() >= 720,
        _ => false,
    }
}

/// BT.2020 frames that go through the tone map. On Windows full-range HLG and SDR, which DXGI
/// can't describe, stayed with the video processor; the shader takes any range.
fn wide_signal(frame: &ff::frame::Video) -> Option<Signal> {
    use ff::color::TransferCharacteristic as Transfer;
    match frame.color_transfer_characteristic() {
        Transfer::SMPTE2084 => Some(Signal::Pq),
        Transfer::ARIB_STD_B67 => Some(Signal::Hlg),
        _ if is_bt2020(frame) => Some(Signal::Sdr),
        _ => None,
    }
}

fn is_bt2020(frame: &ff::frame::Video) -> bool {
    use ff::color::{Primaries, Space};
    matches!(frame.color_space(), Space::BT2020NCL | Space::BT2020CL)
        || frame.color_primaries() == Primaries::BT2020
}

/// libavutil's AVContentLightMetadata (CTA-861.3), which the bindings leave out.
#[repr(C)]
struct ContentLight {
    #[allow(non_snake_case)]
    MaxCLL: u32,
    #[allow(non_snake_case)]
    MaxFALL: u32,
}

/// libavutil's AVMasteringDisplayMetadata (SMPTE ST 2086), which the bindings leave out.
#[repr(C)]
struct MasteringDisplay {
    display_primaries: [[ffi::AVRational; 2]; 3],
    white_point: [ffi::AVRational; 2],
    min_luminance: ffi::AVRational,
    max_luminance: ffi::AVRational,
    has_primaries: i32,
    has_luminance: i32,
}

/// The brightest light of a PQ frame in nits, for the tone map: the stream's MaxCLL, else its
/// mastering display's peak, else 1000.
fn hdr_peak(frame: &ff::frame::Video) -> f32 {
    use ffi::AVFrameSideDataType::*;
    unsafe {
        let light = ffi::av_frame_get_side_data(frame.as_ptr(), AV_FRAME_DATA_CONTENT_LIGHT_LEVEL);
        if !light.is_null() {
            let m = &*((*light).data as *const ContentLight);
            if m.MaxCLL > 0 {
                return m.MaxCLL as f32;
            }
        }
        let display =
            ffi::av_frame_get_side_data(frame.as_ptr(), AV_FRAME_DATA_MASTERING_DISPLAY_METADATA);
        if !display.is_null() {
            let m = &*((*display).data as *const MasteringDisplay);
            if m.has_luminance != 0 && m.max_luminance.den > 0 {
                return m.max_luminance.num as f32 / m.max_luminance.den as f32;
            }
        }
    }
    1000.0
}

/// A frame's colours, as the shader needs them.
struct Colors {
    /// The matrix's Kr and Kb.
    coefficients: (f32, f32),
    full: bool,
    /// Where chroma samples sit, in chroma samples right and down of the texture's even spread.
    siting: (f32, f32),
    /// BT.2020 frames, which are tone mapped.
    signal: Option<Signal>,
    /// A PQ stream's peak in nits.
    peak: f32,
}

impl Colors {
    /// HDR10 and HLG, which an EDR screen shows brighter than SDR white.
    fn hdr(&self) -> bool {
        self.signal.is_some_and(Signal::is_hdr)
    }
}

fn colors(frame: &ff::frame::Video) -> Colors {
    use ff::chroma::Location;
    let signal = wide_signal(frame);
    // PQ is BT.2020 by definition, whatever the stream says.
    let coefficients = if signal == Some(Signal::Pq) || is_bt2020(frame) {
        (0.2627, 0.0593)
    } else if is_bt709(frame) {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let siting = match frame.chroma_location() {
        Location::Center => (0.0, 0.0),
        Location::TopLeft => (0.25, 0.25),
        Location::Top => (0.0, 0.25),
        Location::BottomLeft => (0.25, -0.25),
        Location::Bottom => (0.0, -0.25),
        // Left, and unspecified, which is left for MPEG-2, H.264 and HEVC.
        _ => (0.25, 0.0),
    };
    Colors {
        coefficients,
        full: frame.color_range() == ff::color::Range::JPEG,
        siting,
        signal,
        peak: if signal == Some(Signal::Pq) {
            hdr_peak(frame)
        } else {
            0.0
        },
    }
}

/// Where a frame lands: the visible part of the picture in target pixels, and the point of the
/// frame, in its pixels, at each of that rectangle's corners (top-left, top-right, bottom-left,
/// bottom-right). Rotation and mirroring only change which corner reads which point.
#[derive(Debug)]
struct Placement {
    dest: Px,
    source: [[f64; 2]; 4],
}

/// Where a `w` x `h` frame shows in `area`, None when none of it is visible. For Fill and
/// zoomed-in Scale the picture is bigger than `area`; it is cut to `area`, and what it reads
/// shrinks to match.
fn geometry(w: u32, h: u32, sar: f64, adjust: &VideoAdjust, area: Px) -> Option<Placement> {
    let ((sx, sy, sw, sh), (pw, ph)) = shown(w, h, sar, adjust);
    let dest = place(pw, ph, adjust.zoom, area);
    let visible = Px {
        left: dest.left.max(area.left),
        top: dest.top.max(area.top),
        right: dest.right.min(area.right),
        bottom: dest.bottom.min(area.bottom),
    };
    if visible.right <= visible.left || visible.bottom <= visible.top {
        return None;
    }
    // The visible corners as fractions of the whole picture on screen, then of the cropped
    // source.
    let (dw, dh) = (
        f64::from(dest.right - dest.left),
        f64::from(dest.bottom - dest.top),
    );
    let corner = |x: i32, y: i32| {
        let (u, v) = (f64::from(x - dest.left) / dw, f64::from(y - dest.top) / dh);
        let (u, v) = to_source(u, v, adjust.rotate, adjust.flip);
        [sx + sw * u, sy + sh * v]
    };
    Some(Placement {
        dest: visible,
        source: [
            corner(visible.left, visible.top),
            corner(visible.right, visible.top),
            corner(visible.left, visible.bottom),
            corner(visible.right, visible.bottom),
        ],
    })
}

/// The centred crop of a `w` x `h` frame as (x, y, width, height) in source pixels, and the
/// size it shows at, in source pixel heights, after aspect ratio and rotation.
fn shown(w: u32, h: u32, sar: f64, adjust: &VideoAdjust) -> ((f64, f64, f64, f64), (f64, f64)) {
    let (fw, fh) = (f64::from(w), f64::from(h));
    // Displayed width of one source pixel, relative to its height.
    let par = match adjust.aspect {
        Some((n, d)) if n > 0 && d > 0 => f64::from(n) / f64::from(d) * fh / fw,
        _ => sar,
    };
    // The centred crop, in source pixels.
    let (mut sw, mut sh) = (fw, fh);
    if let Some((n, d)) = adjust.crop.filter(|&(n, d)| n > 0 && d > 0) {
        let ratio = f64::from(n) / f64::from(d);
        if ratio > fw * par / fh {
            sh = fw * par / ratio;
        } else {
            sw = fh * ratio / par;
        }
    }
    let (sx, sy) = ((fw - sw) / 2.0, (fh - sh) / 2.0);
    let quarter = matches!(adjust.rotate, 90 | 270);
    let shown = if quarter {
        (sh, sw * par)
    } else {
        (sw * par, sh)
    };
    ((sx, sy, sw, sh), shown)
}

/// The frame's sample aspect ratio, 1 when the stream doesn't say.
fn sample_aspect(frame: &ff::frame::Video) -> f64 {
    let sar = frame.aspect_ratio();
    if sar.numerator() > 0 && sar.denominator() > 0 {
        f64::from(sar)
    } else {
        1.0
    }
}

/// Maps a point on screen, as fractions of the shown picture, back to fractions of the source:
/// undoes the mirroring, then the clockwise rotation.
fn to_source(u: f64, v: f64, rotate: u16, flip: Flip) -> (f64, f64) {
    let (u, v) = match flip {
        Flip::None => (u, v),
        Flip::Horizontal => (1.0 - u, v),
        Flip::Vertical => (u, 1.0 - v),
    };
    match rotate {
        90 => (v, 1.0 - u),
        180 => (1.0 - u, 1.0 - v),
        270 => (1.0 - v, u),
        _ => (u, v),
    }
}

/// Rectangle with the picture's aspect ratio, centred in `area`. Original is the largest that fits
/// (letterboxed); Fill and Scale above 1 extend past `area`.
fn place(w: f64, h: f64, zoom: Zoom, area: Px) -> Px {
    let (out_w, out_h) = (area.right - area.left, area.bottom - area.top);
    let (fit_w, fit_h) = (f64::from(out_w) / w, f64::from(out_h) / h);
    let scale = match zoom {
        Zoom::Original => fit_w.min(fit_h),
        Zoom::Fill => fit_w.max(fit_h),
        Zoom::Scale(s) => fit_w.min(fit_h) * f64::from(s.max(0.01)),
    };
    let (dw, dh) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let (x, y) = (area.left + (out_w - dw) / 2, area.top + (out_h - dh) / 2);
    Px {
        left: x,
        top: y,
        right: x + dw,
        bottom: y + dh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_centres_the_picture() {
        let square = Px {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 1000,
        };
        let r = place(1920.0, 1080.0, Zoom::Original, square); // 16:9 into a square: bars top and bottom
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 218, 1000, 781));
        let wide = Px {
            left: 10,
            top: 41,
            right: 1930,
            bottom: 1121,
        };
        let r = place(1440.0, 1080.0, Zoom::Original, wide); // 4:3 into a 16:9 area that starts at (10, 41)
        assert_eq!((r.left, r.top, r.right, r.bottom), (250, 41, 1690, 1121));
    }

    const FULL_HD: Px = Px {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    /// What a frame shown in a 1920x1080 area reads, rounded to whole source pixels, and where it
    /// lands, as tuples.
    fn rects(w: u32, h: u32, adjust: VideoAdjust) -> [(i32, i32, i32, i32); 2] {
        let p = geometry(w, h, 1.0, &adjust, FULL_HD).unwrap();
        let xs = p.source.map(|c| c[0]);
        let ys = p.source.map(|c| c[1]);
        let lo = |v: [f64; 4]| v.iter().copied().fold(f64::MAX, f64::min).round() as i32;
        let hi = |v: [f64; 4]| v.iter().copied().fold(f64::MIN, f64::max).round() as i32;
        let d = p.dest;
        [
            (lo(xs), lo(ys), hi(xs), hi(ys)),
            (d.left, d.top, d.right, d.bottom),
        ]
    }

    #[test]
    fn crop_keeps_the_centre_band() {
        let adjust = VideoAdjust {
            crop: Some((239, 100)),
            ..Default::default()
        };
        // 1920 / 2.39 = 803 rows, centred, shown at 1:1 between bars.
        let [source, dest] = rects(1920, 1080, adjust);
        assert_eq!(source, (0, 138, 1920, 942));
        assert_eq!(dest, (0, 138, 1920, 941));
    }

    #[test]
    fn forced_aspect_overrides_the_stream() {
        let adjust = VideoAdjust {
            aspect: Some((4, 3)),
            ..Default::default()
        };
        let [source, dest] = rects(1920, 1080, adjust);
        assert_eq!(source, (0, 0, 1920, 1080));
        assert_eq!(dest, (240, 0, 1680, 1080));
    }

    #[test]
    fn fill_shrinks_the_source_instead_of_overflowing() {
        let adjust = VideoAdjust {
            zoom: Zoom::Fill,
            ..Default::default()
        };
        // 4:3 covering 16:9 is 1920x1440; the middle 1080 rows of that are source rows 135..945.
        let [source, dest] = rects(1440, 1080, adjust);
        assert_eq!(source, (0, 135, 1440, 945));
        assert_eq!(dest, (0, 0, 1920, 1080));
    }

    #[test]
    fn quarter_turns_swap_the_aspect() {
        let adjust = VideoAdjust {
            rotate: 90,
            ..Default::default()
        };
        // Upright 1080x1920 fitted to 1080 rows is 607.5 wide.
        let [source, dest] = rects(1920, 1080, adjust);
        assert_eq!(source, (0, 0, 1920, 1080));
        assert_eq!(dest, (656, 0, 1264, 1080));
        // Filling with the turned picture cuts its top and bottom, which are source columns.
        let [source, dest] = rects(
            1920,
            1080,
            VideoAdjust {
                zoom: Zoom::Fill,
                ..adjust
            },
        );
        assert_eq!(source, (656, 0, 1263, 1080));
        assert_eq!(dest, (0, 0, 1920, 1080));
    }

    #[test]
    fn scale_zooms_into_the_centre() {
        let adjust = VideoAdjust {
            zoom: Zoom::Scale(2.0),
            ..Default::default()
        };
        let [source, dest] = rects(1920, 1080, adjust);
        assert_eq!(source, (480, 270, 1440, 810));
        assert_eq!(dest, (0, 0, 1920, 1080));
        // Zoomed out, the picture floats in the middle of the area.
        let [source, dest] = rects(
            1920,
            1080,
            VideoAdjust {
                zoom: Zoom::Scale(0.5),
                ..adjust
            },
        );
        assert_eq!(source, (0, 0, 1920, 1080));
        assert_eq!(dest, (480, 270, 1440, 810));
    }

    #[test]
    fn mirroring_applies_after_rotation() {
        // Turned clockwise then mirrored left to right, the source's top-left corner shows at the
        // screen's top-left, and its top-right at the bottom-left.
        assert_eq!(to_source(0.0, 0.0, 90, Flip::Horizontal), (0.0, 0.0));
        assert_eq!(to_source(0.0, 1.0, 90, Flip::Horizontal), (1.0, 0.0));
        assert_eq!(to_source(0.0, 0.0, 0, Flip::Vertical), (0.0, 1.0));
    }

    // The GPU tests below draw synthetic frames through the real shader and pipeline, offscreen.

    fn gpu() -> Gpu {
        Gpu::new(create_device().unwrap()).unwrap()
    }

    /// An 8-bit NV12 frame, `w` x `h`, whose pixel (x, y) has the YCbCr `f` gives (chroma is
    /// taken at even pixels).
    fn nv12(w: u32, h: u32, f: impl Fn(u32, u32) -> [u16; 3]) -> ff::frame::Video {
        let mut frame = ff::frame::Video::new(Pixel::NV12, w, h);
        let (ys, cs) = (frame.stride(0), frame.stride(1));
        for y in 0..h {
            for x in 0..w {
                let [l, cb, cr] = f(x, y);
                frame.data_mut(0)[y as usize * ys + x as usize] = l as u8;
                if x % 2 == 0 && y % 2 == 0 {
                    let at = (y / 2) as usize * cs + x as usize;
                    frame.data_mut(1)[at..at + 2].copy_from_slice(&[cb as u8, cr as u8]);
                }
            }
        }
        frame
    }

    /// The same in P010: 10-bit codes at the top of 16 bits.
    fn p010(w: u32, h: u32, f: impl Fn(u32, u32) -> [u16; 3]) -> ff::frame::Video {
        let mut frame = ff::frame::Video::new(Pixel::P010LE, w, h);
        let (ys, cs) = (frame.stride(0), frame.stride(1));
        for y in 0..h {
            for x in 0..w {
                let [l, cb, cr] = f(x, y).map(|v| v << 6);
                let at = y as usize * ys + x as usize * 2;
                frame.data_mut(0)[at..at + 2].copy_from_slice(&l.to_le_bytes());
                if x % 2 == 0 && y % 2 == 0 {
                    let at = (y / 2) as usize * cs + x as usize * 2;
                    let both = [cb.to_le_bytes(), cr.to_le_bytes()].concat();
                    frame.data_mut(1)[at..at + 4].copy_from_slice(&both);
                }
            }
        }
        frame
    }

    /// Draws `frame` at its own size; returns RGB per pixel, 0..1 for sRGB or EDR values for
    /// `screen`.
    fn draw(
        gpu: &mut Gpu,
        frame: &ff::frame::Video,
        adjust: &VideoAdjust,
        screen: Option<Screen>,
    ) -> Vec<[f32; 3]> {
        let size = (frame.width(), frame.height());
        if screen.is_some() {
            let bytes = gpu
                .render(frame, adjust, size, MTLPixelFormat::RGBA16Float, screen)
                .unwrap();
            let halves: Vec<f32> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|h| half(u16::from_le_bytes(*h)))
                .collect();
            halves
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| [p[0], p[1], p[2]])
                .collect()
        } else {
            let bytes = gpu
                .render(frame, adjust, size, MTLPixelFormat::BGRA8Unorm, None)
                .unwrap();
            (bytes.as_chunks::<4>().0.iter())
                .map(|&[b, g, r, _]| [r, g, b].map(|v| f32::from(v) / 255.0))
                .collect()
        }
    }

    fn half(h: u16) -> f32 {
        let sign = if h >> 15 == 1 { -1.0 } else { 1.0 };
        let (e, m) = (i32::from(h >> 10 & 0x1f), f32::from(h & 0x3ff));
        match e {
            0 => sign * m * 2f32.powi(-24),
            _ => sign * (1.0 + m / 1024.0) * 2f32.powi(e - 15),
        }
    }

    /// Exact YCbCr to R'G'B' for 8-bit studio or full range, in 0..1.
    fn exact(ycc: [u16; 3], (kr, kb): (f64, f64), full: bool, bits: i32) -> [f32; 3] {
        let step = 2f64.powi(bits - 8);
        let max = 2f64.powi(bits) - 1.0;
        let [l, cb, cr] = ycc.map(f64::from);
        let (y, cb, cr) = if full {
            (
                l / max,
                (cb - 128.0 * step) / max,
                (cr - 128.0 * step) / max,
            )
        } else {
            (
                (l - 16.0 * step) / (219.0 * step),
                (cb - 128.0 * step) / (224.0 * step),
                (cr - 128.0 * step) / (224.0 * step),
            )
        };
        let kg = 1.0 - kr - kb;
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / kg;
        [r, g, b].map(|v| v.clamp(0.0, 1.0) as f32)
    }

    /// Swatches 16 pixels wide, so each pixel's centre reads one colour however the shader
    /// filters.
    const SWATCHES: [[u16; 3]; 8] = [
        [235, 128, 128],
        [16, 128, 128],
        [126, 128, 128],
        [63, 102, 240],
        [173, 42, 26],
        [32, 240, 118],
        [81, 90, 240],
        [145, 54, 34],
    ];

    fn swatch(x: u32) -> usize {
        (x / 16) as usize % SWATCHES.len()
    }

    /// Checks the pixel at the middle of each swatch against `expected`, within `levels` of 255.
    fn check(out: &[[f32; 3]], w: u32, expected: impl Fn(usize) -> [f32; 3], levels: f32) {
        let mut worst = 0f32;
        for i in 0..SWATCHES.len() {
            let x = i as u32 * 16 + 8;
            let got = out[(8 * w + x) as usize];
            let want = expected(i);
            for c in 0..3 {
                worst = worst.max((got[c] - want[c]).abs() * 255.0);
            }
            assert!(
                (0..3).all(|c| (got[c] - want[c]).abs() * 255.0 <= levels),
                "swatch {i}: {got:?}, want {want:?}"
            );
        }
        eprintln!("worst: {worst:.2} levels of 255");
    }

    #[test]
    fn nv12_matches_the_exact_conversion() {
        let mut gpu = gpu();
        let w = SWATCHES.len() as u32 * 16;
        let mut frame = nv12(w, 16, |x, _| SWATCHES[swatch(x)]);
        frame.set_color_space(ff::color::Space::BT709);
        let out = draw(&mut gpu, &frame, &VideoAdjust::default(), None);
        check(
            &out,
            w,
            |i| exact(SWATCHES[i], (0.2126, 0.0722), false, 8),
            1.0,
        );

        // Full-range BT.601, as JPEG and webcams send it.
        frame.set_color_space(ff::color::Space::BT470BG);
        frame.set_color_range(ff::color::Range::JPEG);
        let out = draw(&mut gpu, &frame, &VideoAdjust::default(), None);
        check(
            &out,
            w,
            |i| exact(SWATCHES[i], (0.299, 0.114), true, 8),
            1.0,
        );
    }

    #[test]
    fn p010_matches_the_exact_conversion() {
        let mut gpu = gpu();
        let w = SWATCHES.len() as u32 * 16;
        let ten = |v: [u16; 3]| v.map(|c| c * 4 + 1);
        let mut frame = p010(w, 16, |x, _| ten(SWATCHES[swatch(x)]));
        frame.set_color_space(ff::color::Space::BT709);
        let out = draw(&mut gpu, &frame, &VideoAdjust::default(), None);
        check(
            &out,
            w,
            |i| exact(ten(SWATCHES[i]), (0.2126, 0.0722), false, 10),
            1.0,
        );
    }

    /// HDR10, HLG and SDR BT.2020 through the shader against tonemap.rs's reference, on an SDR
    /// screen and on an EDR one with 1000 nits of headroom, and the Windows build's numbers.
    #[test]
    fn bt2020_matches_the_tone_map() {
        use tonemap::{Signal, pq_signal, reference};
        let mut gpu = gpu();
        // Grey at 100 nits of PQ, BT.2020 red at 400, and HLG's reference white.
        let code = |v: f32| (64.0 + v * 876.0).round() as u16;
        let colours: [[f32; 3]; 4] = [
            [pq_signal(100.0); 3],
            [pq_signal(400.0), 0.0, 0.0],
            [0.75; 3],
            [842.0 / 1023.0, 336.0 / 1023.0, 185.0 / 1023.0],
        ];
        // R'G'B' to studio-range BT.2020 YCbCr in 10 bits.
        let ycc = |[r, g, b]: [f32; 3]| {
            let y = 0.2627 * r + 0.6780 * g + 0.0593 * b;
            let cb = (b - y) / 1.8814;
            let cr = (r - y) / 1.4746;
            [
                code(y),
                (512.0 + cb * 896.0).round() as u16,
                (512.0 + cr * 896.0).round() as u16,
            ]
        };
        // What the shader decodes from those codes, for the reference.
        let decoded = |c: [u16; 3]| exact(c, (0.2627, 0.0593), false, 10);
        let w = 4 * 16;
        let mut frame = p010(w, 16, |x, _| ycc(colours[(x / 16) as usize]));
        frame.set_color_space(ff::color::Space::BT2020NCL);
        frame.set_color_primaries(ff::color::Primaries::BT2020);
        let screen = Screen {
            peak: 1000.0,
            white: 203.0,
        };
        for (signal, transfer, swatches) in [
            (
                Signal::Pq,
                ff::color::TransferCharacteristic::SMPTE2084,
                0..2,
            ),
            (
                Signal::Hlg,
                ff::color::TransferCharacteristic::ARIB_STD_B67,
                2..3,
            ),
            (
                Signal::Sdr,
                ff::color::TransferCharacteristic::BT2020_10,
                3..4,
            ),
        ] {
            frame.set_color_transfer_characteristic(transfer);
            for screen in [None, Some(screen)] {
                let out = draw(&mut gpu, &frame, &VideoAdjust::default(), screen);
                for i in swatches.clone() {
                    let got = out[(8 * w + i as u32 * 16 + 8) as usize];
                    let want = reference(signal, decoded(ycc(colours[i])), 1000.0, screen);
                    // A few levels of 255 in sRGB; in EDR, 1% of the value or 1/255 of SDR white.
                    let ok = (0..3).all(|c| {
                        let d = (got[c] - want[c]).abs();
                        match screen {
                            None => d * 255.0 <= 2.0,
                            Some(_) => d <= (want[c].abs() * 0.01).max(1.0 / 255.0),
                        }
                    });
                    assert!(
                        ok,
                        "{signal:?} {screen:?} swatch {i}: {got:?}, want {want:?}"
                    );
                }
                let first = out[(8 * w + swatches.start as u32 * 16 + 8) as usize][0];
                eprintln!("{signal:?} on {screen:?}: {first:.4}");
            }
        }
        // The Windows build's numbers: 100 nits of PQ is 0.726 in sRGB and 100 nits on the EDR
        // screen; HLG's reference white is SDR white there.
        frame.set_color_transfer_characteristic(ff::color::TransferCharacteristic::SMPTE2084);
        let sdr = draw(&mut gpu, &frame, &VideoAdjust::default(), None)[(8 * w + 8) as usize];
        let edr =
            draw(&mut gpu, &frame, &VideoAdjust::default(), Some(screen))[(8 * w + 8) as usize];
        assert!((sdr[0] - 0.726).abs() < 0.01, "{sdr:?}");
        assert!((edr[0] * 203.0 - 100.0).abs() < 2.0, "{edr:?}");
    }

    #[test]
    fn adjustments_change_the_picture_as_vlc_does() {
        let mut gpu = gpu();
        let w = SWATCHES.len() as u32 * 16;
        let mut frame = nv12(w, 16, |x, _| SWATCHES[swatch(x)]);
        frame.set_color_space(ff::color::Space::BT709);
        let kr_kb = (0.2126, 0.0722);
        // No saturation leaves each swatch's grey.
        let grey = VideoAdjust {
            saturation: -1.0,
            ..Default::default()
        };
        let out = draw(&mut gpu, &frame, &grey, None);
        check(
            &out,
            w,
            |i| exact([SWATCHES[i][0], 128, 128], kr_kb, false, 8),
            1.0,
        );
        // Double contrast doubles luma from black; the chroma is untouched.
        let contrast = VideoAdjust {
            contrast: 1.0,
            ..Default::default()
        };
        let out = draw(&mut gpu, &frame, &contrast, None);
        check(
            &out,
            w,
            |i| {
                let [y, cb, cr] = SWATCHES[i];
                exact([16 + 2 * (y - 16), cb, cr], kr_kb, false, 8)
            },
            1.0,
        );
        // Half a turn of hue swaps the chroma's signs.
        let hue = VideoAdjust {
            hue: 1.0,
            ..Default::default()
        };
        let out = draw(&mut gpu, &frame, &hue, None);
        check(
            &out,
            w,
            |i| {
                let [y, cb, cr] = SWATCHES[i];
                exact([y, 256 - cb, 256 - cr], kr_kb, false, 8)
            },
            1.0,
        );
    }

    /// The interface, drawn by Core Graphics into the canvas, goes over the video premultiplied
    /// and the right way up, in sRGB and in EDR.
    #[test]
    fn the_interface_goes_over_the_video() {
        use objc2_core_foundation::{CGPoint, CGRect};
        let mut gpu = gpu();
        let size = (64, 32);
        let canvas = Canvas::new(&gpu.device, size).unwrap();
        canvas.clear();
        // Half-transparent white over the bottom half; Core Graphics counts y from the bottom.
        CGContext::set_rgb_fill_color(Some(&canvas.cg), 1.0, 1.0, 1.0, 0.5);
        CGContext::fill_rect(
            Some(&canvas.cg),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(64.0, 16.0)),
        );
        CGContext::flush(Some(&canvas.cg));
        // Under it, BT.709 black.
        let mut frame = nv12(64, 32, |_, _| [16, 128, 128]);
        frame.set_color_space(ff::color::Space::BT709);
        let area = Px {
            left: 0,
            top: 0,
            right: 64,
            bottom: 32,
        };
        for (format, edr) in [
            (MTLPixelFormat::BGRA8Unorm, false),
            (MTLPixelFormat::RGBA16Float, true),
        ] {
            let target = gpu
                .texture(format, size, MTLTextureUsage::RenderTarget)
                .unwrap();
            let video = gpu.prepare(&frame, &VideoAdjust::default(), area, &mut None);
            let video = video.unwrap().unwrap();
            let (video_pipe, ui_pipe) = gpu.pipelines(format).unwrap();
            let commands = gpu.queue.commandBuffer().unwrap();
            let encoder = begin_pass(&commands, &target).unwrap();
            let screen = edr.then_some(Screen {
                peak: 1000.0,
                white: 203.0,
            });
            gpu.encode(&encoder, &video_pipe, &video, screen, size);
            encode_interface(&encoder, &ui_pipe, &canvas.texture, edr);
            encoder.endEncoding();
            commands.commit();
            commands.waitUntilCompleted();
            let pixel = if edr { 8 } else { 4 };
            let mut bytes = vec![0u8; 64 * 32 * pixel];
            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: 64,
                    height: 32,
                    depth: 1,
                },
            };
            unsafe {
                target.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                    NonNull::new(bytes.as_mut_ptr()).unwrap().cast(),
                    64 * pixel,
                    region,
                    0,
                )
            };
            let red = |y: usize| {
                let i = y * 64 * pixel;
                match edr {
                    // RGBA16Float: red is first.
                    true => half(u16::from_le_bytes([bytes[i], bytes[i + 1]])),
                    // BGRA8: red is third.
                    false => f32::from(bytes[i + 2]) / 255.0,
                }
            };
            // In EDR the blend is in linear light, as Windows' scRGB composition was: half of
            // white is half the light there, and half the sRGB level on an sRGB layer.
            let lit = if edr { 0.5 } else { 128.0 / 255.0 };
            assert!(
                red(4) < 0.01,
                "the top half is the video's black: {}",
                red(4)
            );
            assert!(
                (red(28) - lit).abs() < 0.01,
                "the bottom half is lit: {}",
                red(28)
            );
        }
    }

    /// A 64x32 frame in four colours by quadrant, drawn turned and mirrored; which colour lands in
    /// each corner of the output.
    #[test]
    fn rotation_and_mirroring_move_the_corners() {
        let mut gpu = gpu();
        // Red, green, blue and white quadrants, in BT.709 studio range.
        let [red, green, blue, white] = [
            [63, 102, 240],
            [173, 42, 26],
            [32, 240, 118],
            [235, 128, 128],
        ];
        let frame = nv12(64, 32, |x, y| match (x < 32, y < 16) {
            (true, true) => red,
            (false, true) => green,
            (true, false) => blue,
            (false, false) => white,
        });
        let name = |p: [f32; 3]| match p.map(|v| v > 0.5) {
            [true, false, false] => 'R',
            [false, true, false] => 'G',
            [false, false, true] => 'B',
            [true, true, true] => 'W',
            _ => '?',
        };
        let mut corners = |adjust: VideoAdjust| -> String {
            let ((_, _, _, _), (pw, ph)) = shown(64, 32, 1.0, &adjust);
            let size = (pw as u32, ph as u32);
            let bytes = gpu
                .render(&frame, &adjust, size, MTLPixelFormat::BGRA8Unorm, None)
                .unwrap();
            let at = |x: u32, y: u32| {
                let i = ((y * size.0 + x) * 4) as usize;
                name([bytes[i + 2], bytes[i + 1], bytes[i]].map(|v| f32::from(v) / 255.0))
            };
            let (r, b) = (size.0 - 2, size.1 - 2);
            [at(1, 1), at(r, 1), at(1, b), at(r, b)].iter().collect()
        };
        let turn = |rotate, flip| VideoAdjust {
            rotate,
            flip,
            ..Default::default()
        };
        // Top-left, top-right, bottom-left, bottom-right.
        assert_eq!(corners(turn(0, Flip::None)), "RGBW");
        assert_eq!(corners(turn(90, Flip::None)), "BRWG");
        assert_eq!(corners(turn(180, Flip::None)), "WBGR");
        assert_eq!(corners(turn(270, Flip::None)), "GWRB");
        assert_eq!(corners(turn(0, Flip::Horizontal)), "GRWB");
        assert_eq!(corners(turn(0, Flip::Vertical)), "BWRG");
        // Turned, then mirrored on screen: the source's top-left stays top-left.
        assert_eq!(corners(turn(90, Flip::Horizontal)), "RBGW");
        // A 1:1 crop keeps the middle square, which still has all four colours at its corners.
        let crop = VideoAdjust {
            crop: Some((1, 1)),
            ..Default::default()
        };
        assert_eq!(corners(crop), "RGBW");
    }

    /// A real VideoToolbox decode: the first frame of the test clip comes as a pixel buffer, the
    /// renderer takes it without a copy, and it matches the same frame decoded in software.
    #[test]
    fn videotoolbox_frames_match_software_ones() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/test_tracks.mkv");
        let first = |hw: Option<&crate::player::HwDevice>| {
            let mut input = ff::format::input(path).unwrap();
            let stream = input.streams().best(ff::media::Type::Video).unwrap();
            let index = stream.index();
            let mut decoder =
                crate::player::open_video_decoder(stream.parameters(), stream.time_base(), hw)
                    .unwrap();
            let mut frame = ff::frame::Video::empty();
            for (s, packet) in input.packets() {
                if s.index() == index {
                    decoder.send_packet(&packet).unwrap();
                    if decoder.receive_frame(&mut frame).is_ok() {
                        return frame;
                    }
                }
            }
            decoder.send_eof().unwrap();
            decoder.receive_frame(&mut frame).unwrap();
            frame
        };
        let hw = crate::player::HwDevice::videotoolbox().expect("no VideoToolbox device");
        let hard = first(Some(&hw));
        assert_eq!(hard.format(), Pixel::VIDEOTOOLBOX);
        assert!(takes(&hard));
        let soft = first(None);
        let mut gpu = gpu();
        let adjust = VideoAdjust::default();
        let size = (hard.width(), hard.height());
        let a = gpu
            .render(&hard, &adjust, size, MTLPixelFormat::BGRA8Unorm, None)
            .unwrap();
        let b = gpu
            .render(&soft, &adjust, size, MTLPixelFormat::BGRA8Unorm, None)
            .unwrap();
        let diff: u64 = a
            .iter()
            .zip(&b)
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .sum();
        let mean = diff as f64 / a.len() as f64;
        let lit = a.iter().filter(|&&v| v > 16).count();
        eprintln!(
            "{}x{} {:?}: mean difference {mean:.3} levels",
            size.0,
            size.1,
            soft.format()
        );
        assert!(lit > a.len() / 10, "the hardware frame came out black");
        assert!(mean < 1.0, "mean difference {mean}");
    }
}
