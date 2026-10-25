//! Direct3D 11 presenter. Hardware-decoded textures go straight through the GPU's video processor
//! (YUV to RGB, scaling, letterboxing, video adjustments) into the swap chain, so they never touch
//! system memory. Software frames are repacked to NV12 with swscale and uploaded first, then take
//! the same path. Direct2D then paints the interface into the same back buffer.

use std::error::Error;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::PathBuf;

use ff::ffi;
use ff::format::Pixel;
use ffmpeg_next as ff;
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

use crate::ui::{Browse, Layout, Painter, Rect, Scene};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const BLACK: D3D11_VIDEO_COLOR = D3D11_VIDEO_COLOR {
    Anonymous: D3D11_VIDEO_COLOR_0 {
        RGBA: D3D11_VIDEO_COLOR_RGBA {
            R: 0.0,
            G: 0.0,
            B: 0.0,
            A: 1.0,
        },
    },
};
const RGB_FULL: D3D11_VIDEO_PROCESSOR_COLOR_SPACE =
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };

/// FFmpeg's lock for the device context it shares with us. Held around every immediate-context
/// call so they cannot interleave with the decoder's own sequences.
pub struct DeviceLock {
    lock: unsafe extern "C" fn(*mut c_void),
    unlock: unsafe extern "C" fn(*mut c_void),
    ctx: *mut c_void,
}

impl DeviceLock {
    pub fn from_hw(hw: &ffi::AVD3D11VADeviceContext) -> Option<Self> {
        Some(DeviceLock {
            lock: hw.lock?,
            unlock: hw.unlock?,
            ctx: hw.lock_ctx,
        })
    }
}

fn locked<R>(lock: &Option<DeviceLock>, f: impl FnOnce() -> R) -> R {
    if let Some(l) = lock {
        unsafe { (l.lock)(l.ctx) }
    }
    let r = f();
    if let Some(l) = lock {
        unsafe { (l.unlock)(l.ctx) }
    }
    r
}

/// One device for decoding, video processing and Direct2D. BGRA support is what Direct2D needs.
pub fn create_device() -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device: ID3D11Device = device.ok_or("no D3D11 device")?;
        // The decoder thread and the render thread share the immediate context.
        _ = device
            .GetImmediateContext()?
            .cast::<ID3D11Multithread>()?
            .SetMultithreadProtected(true);
        Ok(device)
    }
}

/// Mirroring of the picture as shown, so after rotation, which is the order the video processor
/// applies them in: Horizontal always swaps the screen's left and right.
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

/// VLC's video adjustments, applied by the GPU's video processor on both decoding paths.
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

/// The video processor's colour filters, in VideoAdjust order.
const FILTERS: [(
    D3D11_VIDEO_PROCESSOR_FILTER,
    D3D11_VIDEO_PROCESSOR_FILTER_CAPS,
); 4] = [
    (
        D3D11_VIDEO_PROCESSOR_FILTER_BRIGHTNESS,
        D3D11_VIDEO_PROCESSOR_FILTER_CAPS_BRIGHTNESS,
    ),
    (
        D3D11_VIDEO_PROCESSOR_FILTER_CONTRAST,
        D3D11_VIDEO_PROCESSOR_FILTER_CAPS_CONTRAST,
    ),
    (
        D3D11_VIDEO_PROCESSOR_FILTER_HUE,
        D3D11_VIDEO_PROCESSOR_FILTER_CAPS_HUE,
    ),
    (
        D3D11_VIDEO_PROCESSOR_FILTER_SATURATION,
        D3D11_VIDEO_PROCESSOR_FILTER_CAPS_SATURATION,
    ),
];

struct Processor {
    /// Input width, height and format, then output size.
    key: (u32, u32, DXGI_FORMAT, (u32, u32)),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    rotation: bool,
    mirror: bool,
    /// Range of each colour filter the driver supports, in FILTERS order.
    filters: [Option<D3D11_VIDEO_PROCESSOR_FILTER_RANGE>; 4],
}

/// Software frames go to the GPU as NV12, so the video processor converts them to RGB and applies
/// the colour filters exactly as for hardware frames. Drivers skip those filters on RGB input.
struct Upload {
    key: (Pixel, u32, u32),
    scaler: ff::software::scaling::Context,
    nv12: ff::frame::Video,
    /// CPU-writable copy, then the texture the video processor reads.
    staging: ID3D11Texture2D,
    texture: ID3D11Texture2D,
    space: D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    /// Mirroring lives on this Windows 8.1 interface.
    video_context1: Option<ID3D11VideoContext1>,
    swapchain: IDXGISwapChain1,
    // Fields drop in order: the painter's brushes, icons and bitmaps must go before their device
    // context, or Direct2D crashes on exit.
    painter: Painter,
    d2d: ID2D1DeviceContext5,
    lock: Option<DeviceLock>,
    /// The colours the painter's resources were made in.
    theme: crate::ui::theme::Theme,
    size: (u32, u32),
    processor: Option<Processor>,
    upload: Option<Upload>,
    /// Set once an adjustment the GPU can't do has been reported.
    warned: bool,
}

impl Renderer {
    pub fn new(
        hwnd: HWND,
        size: (u32, u32),
        device: ID3D11Device,
        lock: Option<DeviceLock>,
    ) -> Result<Self> {
        unsafe {
            let context = device.GetImmediateContext()?;
            let dxgi: IDXGIDevice = device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0,
                Height: size.1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                Scaling: DXGI_SCALING_NONE,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swapchain = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            // Alt+Enter would switch to exclusive fullscreen; F toggles borderless fullscreen instead.
            factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;

            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d: ID2D1DeviceContext5 = d2d_factory
                .CreateDevice(&dxgi)?
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?
                .cast()?;
            d2d.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let painter = Painter::new(&d2d, d2d_factory)?;
            Ok(Renderer {
                video_device: device.cast()?,
                video_context: context.cast()?,
                video_context1: context.cast().ok(),
                device,
                context,
                swapchain,
                d2d,
                painter,
                lock,
                theme: crate::ui::theme::current(),
                size,
                processor: None,
                upload: None,
                warned: false,
            })
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 || (width, height) == self.size {
            return Ok(());
        }
        unsafe {
            // Views on the old back buffer are already dropped; flush so D3D releases them too.
            locked(&self.lock, || self.context.Flush());
            self.swapchain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.size = (width, height);
        Ok(())
    }

    pub fn library_layout(&self, width: f32, height: f32, browse: &Browse, card: bool) -> Layout {
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

    pub fn take_wanted(&mut self) -> Vec<PathBuf> {
        self.painter.take_wanted()
    }

    /// Draws the video frame (if any) into the scene's video area, paints the interface around
    /// and over it, and presents. `dpi` converts the scene's DIPs to back-buffer pixels.
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
        let backbuffer: ID3D11Texture2D = unsafe { self.swapchain.GetBuffer(0)? };
        let area = to_pixels(scene.layout.video.scaled(dpi / 96.0));
        let picture = match frame {
            Some(frame) => self.blit(frame, &backbuffer, area, adjust)?,
            None => None,
        };
        let px = 96.0 / dpi;
        self.painter.picture = picture.map(|r| {
            Rect::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32).scaled(px)
        });
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: dpi,
                dpiY: dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            // ponytail: a target bitmap per present; cache one per swap-chain buffer if it shows up in a profile.
            let target = self
                .d2d
                .CreateBitmapFromDxgiSurface(&backbuffer.cast::<IDXGISurface>()?, Some(&props))?;
            let (d2d, painter) = (&self.d2d, &mut self.painter);
            locked(&self.lock, || -> Result<()> {
                d2d.SetTarget(&target);
                d2d.SetDpi(dpi, dpi);
                d2d.BeginDraw();
                let painted = painter.paint(d2d, scene);
                let ended = d2d.EndDraw(None, None);
                d2d.SetTarget(None);
                painted?;
                Ok(ended?)
            })?;
            // Sync interval 1 waits for vblank, so frames land without tearing.
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Converts one frame into `area` of the back buffer, adjusted and letterboxed. Returns where
    /// the picture landed.
    fn blit(
        &mut self,
        frame: &ff::frame::Video,
        backbuffer: &ID3D11Texture2D,
        area: RECT,
        adjust: &VideoAdjust,
    ) -> Result<Option<RECT>> {
        let hardware = frame.format() == Pixel::D3D11;
        let (texture, slice, input_space) = if hardware {
            // D3D11 frames carry the decoder's texture array in data[0] and the slice index in data[1].
            unsafe {
                let raw = frame.as_ptr();
                let texture = (*raw).data[0] as *mut c_void;
                let texture =
                    ID3D11Texture2D::from_raw_borrowed(&texture).ok_or("frame has no texture")?;
                let slice = (*raw).data[1] as usize as u32;
                (texture.clone(), slice, ycbcr_space(frame))
            }
        } else {
            let (texture, space) = self.upload(frame)?;
            (texture, 0, space)
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let (w, h) = (frame.width(), frame.height());
        self.ensure_processor(w, h, desc.Format)?;
        let p = self.processor.as_ref().unwrap();

        // Drop what this GPU can't do, so the geometry matches what it draws.
        let mirror = self.video_context1.as_ref().filter(|_| p.mirror);
        let mut adjust = *adjust;
        let mut missing = Vec::new();
        if !p.rotation && matches!(adjust.rotate, 90 | 180 | 270) {
            missing.push("rotation");
            adjust.rotate = 0;
        }
        if mirror.is_none() && adjust.flip != Flip::None {
            missing.push("mirroring");
            adjust.flip = Flip::None;
        }
        let levels = [
            adjust.brightness,
            adjust.contrast,
            adjust.hue,
            adjust.saturation,
        ];
        for (name, (range, level)) in ["brightness", "contrast", "hue", "saturation"]
            .into_iter()
            .zip(p.filters.iter().zip(levels))
        {
            if range.is_none() && level != 0.0 {
                missing.push(name);
            }
        }
        if !missing.is_empty() && !self.warned {
            eprintln!("video: this GPU can't do {}", missing.join(", "));
            self.warned = true;
        }
        let sar = frame.aspect_ratio();
        let sar = if sar.numerator() > 0 && sar.denominator() > 0 {
            f64::from(sar)
        } else {
            1.0
        };
        let Some((source, dest)) = geometry(w, h, sar, &adjust, area) else {
            return Ok(None); // nothing of the picture is visible
        };

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: slice,
                },
            },
            ..Default::default()
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let (mut input, mut output) = (None, None);
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                &texture,
                &p.enumerator,
                &input_desc,
                Some(&mut input),
            )?;
            self.video_device.CreateVideoProcessorOutputView(
                backbuffer,
                &p.enumerator,
                &output_desc,
                Some(&mut output),
            )?;
        }
        let output = output.ok_or("no output view")?;

        let rotation = match adjust.rotate {
            90 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
            180 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
            270 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
            _ => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
        };
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(input),
            ..Default::default()
        }];
        let (vc, vp) = (&self.video_context, &p.processor);
        let result = locked(&self.lock, || unsafe {
            // Stream state persists on the processor, so every setting is written on every blit.
            if p.rotation {
                vc.VideoProcessorSetStreamRotation(
                    vp,
                    0,
                    rotation != D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
                    rotation,
                );
            }
            if let Some(vc1) = mirror {
                vc1.VideoProcessorSetStreamMirror(
                    vp,
                    0,
                    adjust.flip != Flip::None,
                    adjust.flip == Flip::Horizontal,
                    adjust.flip == Flip::Vertical,
                );
            }
            for (((filter, _), range), level) in FILTERS.iter().zip(&p.filters).zip(levels) {
                if let Some(range) = range {
                    let on = level != 0.0;
                    let value = if on {
                        filter_level(level, range)
                    } else {
                        range.Default
                    };
                    vc.VideoProcessorSetStreamFilter(vp, 0, *filter, on, value);
                }
            }
            vc.VideoProcessorSetStreamFrameFormat(vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vc.VideoProcessorSetStreamSourceRect(vp, 0, true, Some(&source));
            vc.VideoProcessorSetStreamDestRect(vp, 0, true, Some(&dest));
            // Only the video area; Direct2D paints everything outside it.
            vc.VideoProcessorSetOutputTargetRect(vp, true, Some(&area));
            vc.VideoProcessorSetOutputBackgroundColor(vp, false, &BLACK);
            vc.VideoProcessorSetStreamColorSpace(vp, 0, &input_space);
            vc.VideoProcessorSetOutputColorSpace(vp, &RGB_FULL);
            vc.VideoProcessorBlt(vp, &output, 0, &streams)
        });
        unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
        result?;
        Ok(Some(dest))
    }

    fn ensure_processor(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<()> {
        let key = (w, h, format, self.size);
        if self.processor.as_ref().is_some_and(|p| p.key == key) {
            return Ok(());
        }
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputWidth: w,
            InputHeight: h,
            OutputWidth: self.size.0,
            OutputHeight: self.size.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            ..Default::default()
        };
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            // Drivers may sharpen or denoise by default; show the decoded pixels as they are.
            locked(&self.lock, || {
                self.video_context
                    .VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false)
            });
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            enumerator.GetVideoProcessorCaps(&mut caps)?;
            let feature =
                |f: D3D11_VIDEO_PROCESSOR_FEATURE_CAPS| caps.FeatureCaps & f.0 as u32 != 0;
            let filters = FILTERS.map(|(filter, cap)| {
                if caps.FilterCaps & cap.0 as u32 == 0 {
                    return None;
                }
                enumerator.GetVideoProcessorFilterRange(filter).ok()
            });
            self.processor = Some(Processor {
                key,
                enumerator,
                processor,
                rotation: feature(D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION),
                mirror: feature(D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_MIRROR),
                filters,
            });
        }
        Ok(())
    }

    /// Repacks a software frame into NV12 on the GPU; returns the texture and its colour space.
    fn upload(
        &mut self,
        frame: &ff::frame::Video,
    ) -> Result<(ID3D11Texture2D, D3D11_VIDEO_PROCESSOR_COLOR_SPACE)> {
        let key = (frame.format(), frame.width(), frame.height());
        // NV12 textures need even sizes; the source rect never reads the padding.
        let (tex_w, tex_h) = ((key.1 + 1) & !1, (key.2 + 1) & !1);
        if self.upload.as_ref().is_none_or(|u| u.key != key) {
            let (format, w, h) = key;
            let flags = ff::software::scaling::Flags::BILINEAR;
            let mut scaler =
                ff::software::scaling::Context::get(format, w, h, Pixel::NV12, w, h, flags)?;
            let rgb = unsafe {
                let desc = ffi::av_pix_fmt_desc_get(format.into());
                !desc.is_null() && (*desc).flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0
            };
            // YUV keeps its matrix and range, so swscale only repacks the planes. RGB sources
            // become BT.709 studio range, and the video processor is told so.
            let (space, full_range, matrix) = if rgb {
                let space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: (1 << 2) | (1 << 4),
                };
                (space, 0, ffi::SWS_CS_ITU709)
            } else {
                let full = frame.color_range() == ff::color::Range::JPEG;
                let matrix = if is_bt709(frame) {
                    ffi::SWS_CS_ITU709
                } else {
                    ffi::SWS_CS_DEFAULT
                };
                (ycbcr_space(frame), i32::from(full), matrix)
            };
            unsafe {
                let coefficients = ffi::sws_getCoefficients(matrix);
                ffi::sws_setColorspaceDetails(
                    scaler.as_mut_ptr(),
                    coefficients,
                    if rgb { 1 } else { full_range },
                    coefficients,
                    full_range,
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: tex_w,
                Height: tex_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                // Intel's video processor rejects an input view on a shader-resource NV12 texture.
                BindFlags: D3D11_BIND_DECODER.0 as u32,
                ..Default::default()
            };
            let (mut texture, mut staging) = (None, None);
            unsafe {
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut texture))?;
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE.0 as u32;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut staging))?;
            }
            self.upload = Some(Upload {
                key,
                scaler,
                nv12: ff::frame::Video::empty(),
                staging: staging.ok_or("no staging texture")?,
                texture: texture.ok_or("no upload texture")?,
                space,
            });
        }
        let u = self.upload.as_mut().unwrap();
        u.scaler.run(frame, &mut u.nv12)?;
        let (w, h) = (key.1 as usize, key.2 as usize);
        let (ctx, nv12) = (&self.context, &u.nv12);
        // ponytail: one staging texture, so Map may wait for the previous copy; use two if a profile shows it.
        locked(&self.lock, || -> Result<()> {
            unsafe {
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&u.staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))?;
                let pitch = mapped.RowPitch as usize;
                let base = mapped.pData as *mut u8;
                // The interleaved UV rows start after the texture's full height of Y rows.
                let uv = base.add(pitch * tex_h as usize);
                let planes = [(0, base, h, w), (1, uv, h.div_ceil(2), w.div_ceil(2) * 2)];
                for (plane, dst, rows, bytes) in planes {
                    let (src, stride) = (nv12.data(plane), nv12.stride(plane));
                    for row in 0..rows {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr().add(row * stride),
                            dst.add(row * pitch),
                            bytes,
                        );
                    }
                }
                ctx.Unmap(&u.staging, 0);
                ctx.CopyResource(&u.texture, &u.staging);
            }
            Ok(())
        })?;
        Ok((u.texture.clone(), u.space))
    }
}

fn to_pixels(r: Rect) -> RECT {
    RECT {
        left: r.l.round() as i32,
        top: r.t.round() as i32,
        right: r.r.round() as i32,
        bottom: r.b.round() as i32,
    }
}

/// Untagged HD video is BT.709 by convention and SD is BT.601, the same guess VLC and mpv make.
/// ponytail: BT.2020/HDR maps to BT.601 here; it needs ID3D11VideoContext1 colour spaces.
fn is_bt709(frame: &ff::frame::Video) -> bool {
    match frame.color_space() {
        ff::color::Space::BT709 => true,
        ff::color::Space::Unspecified => frame.height() >= 720,
        _ => false,
    }
}

/// D3D11_VIDEO_PROCESSOR_COLOR_SPACE bits: 2 is YCbCr_Matrix (1 = BT.709), 4-5 are
/// Nominal_Range (1 = studio 16-235, 2 = full 0-255).
fn ycbcr_space(frame: &ff::frame::Video) -> D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    let matrix = u32::from(is_bt709(frame));
    let range = if frame.color_range() == ff::color::Range::JPEG {
        2
    } else {
        1
    };
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
        _bitfield: (matrix << 2) | (range << 4),
    }
}

/// Maps -1..1 onto a filter's range: 0 is the driver's default and the ends are its limits.
fn filter_level(level: f32, range: &D3D11_VIDEO_PROCESSOR_FILTER_RANGE) -> i32 {
    let level = f64::from(level.clamp(-1.0, 1.0));
    let (lo, default, hi) = (
        f64::from(range.Minimum),
        f64::from(range.Default),
        f64::from(range.Maximum),
    );
    // Contrast and saturation are gains around their default; full range is 10x on Intel, so cap
    // the boost at 2x like VLC's adjust filter. Brightness and hue are offsets and keep their range.
    let hi = if lo == 0.0 { hi.min(default * 2.0) } else { hi };
    let value = if level >= 0.0 {
        default + level * (hi - default)
    } else {
        default + level * (default - lo)
    };
    value.round() as i32
}

/// The part of a `w` x `h` frame to read and the part of the back buffer it lands on.
/// None when none of the picture is visible.
///
/// The video processor's source and destination rects carry crop, aspect and zoom; rotation and
/// mirroring are set separately. For Fill and zoomed-in Scale the picture is bigger than `area`,
/// and rather than hand the driver a destination that overflows its target (drivers differ on
/// clipping that), the destination is cut to `area` and the source shrunk to match.
fn geometry(w: u32, h: u32, sar: f64, adjust: &VideoAdjust, area: RECT) -> Option<(RECT, RECT)> {
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
    let (pw, ph) = if quarter {
        (sh, sw * par)
    } else {
        (sw * par, sh)
    };

    let dest = place(pw, ph, adjust.zoom, area);
    let visible = RECT {
        left: dest.left.max(area.left),
        top: dest.top.max(area.top),
        right: dest.right.min(area.right),
        bottom: dest.bottom.min(area.bottom),
    };
    if visible.right <= visible.left || visible.bottom <= visible.top {
        return None;
    }
    // The visible part as fractions of the whole picture on screen, then of the cropped source.
    let (dw, dh) = (
        f64::from(dest.right - dest.left),
        f64::from(dest.bottom - dest.top),
    );
    let corner = |x: i32, y: i32| {
        let (u, v) = (f64::from(x - dest.left) / dw, f64::from(y - dest.top) / dh);
        to_source(u, v, adjust.rotate, adjust.flip)
    };
    let (a, b) = (
        corner(visible.left, visible.top),
        corner(visible.right, visible.bottom),
    );
    // Rounded to whole pixels, at least one wide, inside the frame.
    let edge = |from: f64, size: f64, t0: f64, t1: f64, limit: i32| {
        let lo = (from + size * t0.min(t1)).round() as i32;
        let hi = (from + size * t0.max(t1)).round() as i32;
        let lo = lo.clamp(0, limit - 1);
        (lo, hi.clamp(lo + 1, limit))
    };
    let (left, right) = edge(sx, sw, a.0, b.0, w as i32);
    let (top, bottom) = edge(sy, sh, a.1, b.1, h as i32);
    let source = RECT {
        left,
        top,
        right,
        bottom,
    };
    Some((source, visible))
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
fn place(w: f64, h: f64, zoom: Zoom, area: RECT) -> RECT {
    let (out_w, out_h) = (area.right - area.left, area.bottom - area.top);
    let (fit_w, fit_h) = (f64::from(out_w) / w, f64::from(out_h) / h);
    let scale = match zoom {
        Zoom::Original => fit_w.min(fit_h),
        Zoom::Fill => fit_w.max(fit_h),
        Zoom::Scale(s) => fit_w.min(fit_h) * f64::from(s.max(0.01)),
    };
    let (dw, dh) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let (x, y) = (area.left + (out_w - dw) / 2, area.top + (out_h - dh) / 2);
    RECT {
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
        let square = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 1000,
        };
        let r = place(1920.0, 1080.0, Zoom::Original, square); // 16:9 into a square: bars top and bottom
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 218, 1000, 781));
        let wide = RECT {
            left: 10,
            top: 41,
            right: 1930,
            bottom: 1121,
        };
        let r = place(1440.0, 1080.0, Zoom::Original, wide); // 4:3 into a 16:9 area that starts at (10, 41)
        assert_eq!((r.left, r.top, r.right, r.bottom), (250, 41, 1690, 1121));
    }

    const FULL_HD: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    /// Source and destination of a frame shown in a 1920x1080 area, as tuples.
    fn rects(w: u32, h: u32, adjust: VideoAdjust) -> [(i32, i32, i32, i32); 2] {
        let (s, d) = geometry(w, h, 1.0, &adjust, FULL_HD).unwrap();
        [
            (s.left, s.top, s.right, s.bottom),
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

    #[test]
    fn filter_levels_cap_gains_at_twice_the_default() {
        let range = D3D11_VIDEO_PROCESSOR_FILTER_RANGE {
            Minimum: 0,
            Maximum: 1000,
            Default: 100,
            Multiplier: 0.01,
        };
        assert_eq!(filter_level(0.0, &range), 100);
        assert_eq!(filter_level(-1.0, &range), 0);
        assert_eq!(filter_level(-0.5, &range), 50);
        assert_eq!(filter_level(1.0, &range), 200);
        assert_eq!(filter_level(2.0, &range), 200);
    }
}
