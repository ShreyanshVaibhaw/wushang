//! The render target, its brushes and bitmaps, layers and clips.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ops::Deref;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2_core_foundation::{CFData, CFRetained, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextGetColorSpace, CGBitmapInfo, CGBlendMode, CGColorRenderingIntent, CGColorSpace,
    CGContext, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality, CGLineCap, CGLineJoin, CGMutablePath, CGPath,
};

use super::*;

/// What a push left on the stack, for the matching pop.
enum Pushed {
    Clip,
    Layer { transparency: bool },
}

/// The drawing state behind every view of one device context.
pub(crate) struct DcState {
    cg: Option<CFRetained<CGContext>>,
    /// The bitmap's colour space: colours and images are given in it, so Core Graphics
    /// converts nothing and the bytes come out as Direct2D would write them.
    pub(crate) space: CFRetained<CGColorSpace>,
    /// Pixels per DIP.
    pub(crate) scale: f32,
    size: (u32, u32),
    pub(crate) world: Matrix3x2,
    stack: Vec<Pushed>,
    /// DrawText's layouts, kept while the same strings are drawn frame after frame.
    texts: HashMap<TextKey, (IDWriteTextLayout, u64)>,
    frame: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TextKey {
    format: u64,
    generation: u64,
    text: Vec<u16>,
    width: u32,
    height: u32,
}

pub(crate) fn srgb() -> CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB }))
        .expect("the sRGB colour space")
}

impl DcState {
    /// Runs `f` with the CTM at the world transform, in DIPs.
    pub(crate) fn with_world(&self, f: impl FnOnce(&CGContext)) {
        self.with_transform(&self.world, f);
    }

    pub(crate) fn with_transform(&self, m: &Matrix3x2, f: impl FnOnce(&CGContext)) {
        let Some(cg) = self.cg.as_deref() else { return };
        CGContext::save_g_state(Some(cg));
        CGContext::concat_ctm(Some(cg), m.cg());
        f(cg);
        CGContext::restore_g_state(Some(cg));
    }

    /// The context with the CTM at DIPs, untransformed.
    pub(crate) fn cg(&self) -> Option<&CGContext> {
        self.cg.as_deref()
    }

    pub(crate) fn set_fill(&self, cg: &CGContext, c: &D2D1_COLOR_F, opacity: f32) {
        let parts: [CGFloat; 4] = [
            c.r.into(),
            c.g.into(),
            c.b.into(),
            (c.a * opacity).clamp(0.0, 1.0).into(),
        ];
        CGContext::set_fill_color_space(Some(cg), Some(&self.space));
        unsafe { CGContext::set_fill_color(Some(cg), parts.as_ptr()) };
    }

    fn set_stroke(&self, cg: &CGContext, c: &D2D1_COLOR_F, opacity: f32) {
        let parts: [CGFloat; 4] = [
            c.r.into(),
            c.g.into(),
            c.b.into(),
            (c.a * opacity).clamp(0.0, 1.0).into(),
        ];
        CGContext::set_stroke_color_space(Some(cg), Some(&self.space));
        unsafe { CGContext::set_stroke_color(Some(cg), parts.as_ptr()) };
    }

    /// Fills `path` (in world space) with `paint`.
    pub(crate) fn fill(&self, path: &CGPath, even_odd: bool, paint: &PaintSpec) {
        self.with_world(|cg| {
            CGContext::add_path(Some(cg), Some(path));
            match paint {
                PaintSpec::Solid(c, opacity) => {
                    self.set_fill(cg, c, *opacity);
                    if even_odd {
                        CGContext::eo_fill_path(Some(cg));
                    } else {
                        CGContext::fill_path(Some(cg));
                    }
                }
                PaintSpec::Bitmap(b) => {
                    if even_odd {
                        CGContext::eo_clip(Some(cg));
                    } else {
                        CGContext::clip(Some(cg));
                    }
                    self.paint_bitmap(cg, b);
                }
            }
        });
    }

    /// Strokes `path` (in world space) `width` wide, centred on it.
    pub(crate) fn stroke(
        &self,
        path: &CGPath,
        width: f32,
        style: Option<&ID2D1StrokeStyle>,
        paint: &PaintSpec,
    ) {
        self.with_world(|cg| {
            CGContext::add_path(Some(cg), Some(path));
            CGContext::set_line_width(Some(cg), width.into());
            let props = style.map(|s| s.0.props).unwrap_or_default();
            CGContext::set_line_cap(
                Some(cg),
                match props.startCap {
                    D2D1_CAP_STYLE_ROUND => CGLineCap::Round,
                    D2D1_CAP_STYLE_SQUARE => CGLineCap::Square,
                    _ => CGLineCap::Butt,
                },
            );
            CGContext::set_line_join(
                Some(cg),
                match props.lineJoin {
                    D2D1_LINE_JOIN_ROUND => CGLineJoin::Round,
                    D2D1_LINE_JOIN_BEVEL => CGLineJoin::Bevel,
                    _ => CGLineJoin::Miter,
                },
            );
            CGContext::set_miter_limit(Some(cg), props.miterLimit.max(1.0).into());
            if let Some(dashes) = style.and_then(|s| s.dashes()) {
                // Direct2D's dashes are in stroke widths.
                let lengths: Vec<CGFloat> =
                    dashes.iter().map(|d| CGFloat::from(d * width)).collect();
                unsafe {
                    CGContext::set_line_dash(
                        Some(cg),
                        (props.dashOffset * width).into(),
                        lengths.as_ptr(),
                        lengths.len(),
                    )
                };
            }
            match paint {
                PaintSpec::Solid(c, opacity) => {
                    self.set_stroke(cg, c, *opacity);
                    CGContext::stroke_path(Some(cg));
                }
                PaintSpec::Bitmap(b) => {
                    CGContext::replace_path_with_stroked_path(Some(cg));
                    CGContext::clip(Some(cg));
                    self.paint_bitmap(cg, b);
                }
            }
        });
    }

    /// Paints a bitmap brush over the current clip; the CTM is the world transform.
    fn paint_bitmap(&self, cg: &CGContext, b: &BitmapPaintSpec) {
        let Some(image) = b.bitmap.image(&self.space) else {
            return;
        };
        let size = b.bitmap.GetSize_();
        CGContext::set_alpha(Some(cg), b.opacity.into());
        CGContext::concat_ctm(Some(cg), b.transform.cg());
        CGContext::set_interpolation_quality(Some(cg), quality(b.interpolation));
        draw_image(cg, &image, (0.0, 0.0, size.width, size.height));
    }

    /// A world-space rectangle in device pixels, as its bounding box under the transform.
    fn device_rect(&self, r: &D2D_RECT_F) -> (f32, f32, f32, f32) {
        let r = r.normalized();
        let corners = [
            self.world.transform(r.left, r.top),
            self.world.transform(r.right, r.top),
            self.world.transform(r.left, r.bottom),
            self.world.transform(r.right, r.bottom),
        ];
        let s = self.scale;
        let (mut l, mut t, mut rr, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in corners {
            l = l.min(x * s);
            t = t.min(y * s);
            rr = rr.max(x * s);
            b = b.max(y * s);
        }
        (l, t, rr, b)
    }

    /// Clips to device pixels `(l, t, r, b)` until the matching pop.
    fn clip_device(&mut self, (l, t, r, b): (f32, f32, f32, f32)) {
        let Some(cg) = self.cg.clone() else { return };
        let s = self.scale;
        let c = |v: f32| f64::from(v / s).clamp(-1.0e7, 1.0e7);
        let rect = CGRect::new(
            CGPoint::new(c(l), c(t)),
            CGSize::new((c(r) - c(l)).max(0.0), (c(b) - c(t)).max(0.0)),
        );
        CGContext::clip_to_rect(Some(&cg), rect);
    }
}

/// How a brush paints, copied out of it for one draw.
pub(crate) enum PaintSpec {
    Solid(D2D1_COLOR_F, f32),
    Bitmap(BitmapPaintSpec),
}

pub(crate) struct BitmapPaintSpec {
    bitmap: ID2D1Bitmap,
    transform: Matrix3x2,
    opacity: f32,
    interpolation: D2D1_INTERPOLATION_MODE,
}

fn quality(mode: D2D1_INTERPOLATION_MODE) -> CGInterpolationQuality {
    match mode {
        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR => CGInterpolationQuality::None,
        D2D1_INTERPOLATION_MODE_LINEAR => CGInterpolationQuality::Low,
        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC => CGInterpolationQuality::High,
        _ => CGInterpolationQuality::Medium,
    }
}

/// Draws `image` into the DIP rect `(x, y, w, h)` the right way up under the y-down CTM.
pub(crate) fn draw_image(cg: &CGContext, image: &CGImage, (x, y, w, h): (f32, f32, f32, f32)) {
    CGContext::save_g_state(Some(cg));
    CGContext::translate_ctm(Some(cg), x.into(), f64::from(y + h));
    CGContext::scale_ctm(Some(cg), 1.0, -1.0);
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w.into(), h.into()));
    CGContext::draw_image(Some(cg), rect, Some(image));
    CGContext::restore_g_state(Some(cg));
}

/// A brush argument: any brush by reference.
pub trait BrushParam {
    #[doc(hidden)]
    fn paint(&self) -> PaintSpec;
}

impl BrushParam for &ID2D1SolidColorBrush {
    fn paint(&self) -> PaintSpec {
        PaintSpec::Solid(self.0.color.get(), self.0.opacity.get())
    }
}

impl BrushParam for &ID2D1BitmapBrush {
    fn paint(&self) -> PaintSpec {
        let b = &self.0;
        PaintSpec::Bitmap(BitmapPaintSpec {
            bitmap: b.bitmap.clone(),
            transform: b.transform.get(),
            opacity: b.opacity.get(),
            interpolation: b.interpolation.get(),
        })
    }
}

impl BrushParam for &ID2D1Brush {
    fn paint(&self) -> PaintSpec {
        match self {
            ID2D1Brush::Solid(b) => b.paint(),
            ID2D1Brush::Bitmap(b) => b.paint(),
        }
    }
}

impl<T: BrushParam> BrushParam for &&T {
    fn paint(&self) -> PaintSpec {
        (**self).paint()
    }
}

/// A stroke style argument: `None` or `&style`.
pub trait StrokeParam {
    #[doc(hidden)]
    fn style(&self) -> Option<&ID2D1StrokeStyle>;
}

impl StrokeParam for &ID2D1StrokeStyle {
    fn style(&self) -> Option<&ID2D1StrokeStyle> {
        Some(self)
    }
}

impl StrokeParam for Option<&ID2D1StrokeStyle> {
    fn style(&self) -> Option<&ID2D1StrokeStyle> {
        *self
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_BRUSH_PROPERTIES {
    pub opacity: f32,
    pub transform: Matrix3x2,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_BITMAP_BRUSH_PROPERTIES {
    pub extendModeX: D2D1_EXTEND_MODE,
    pub extendModeY: D2D1_EXTEND_MODE,
    pub interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_BITMAP_BRUSH_PROPERTIES1 {
    pub extendModeX: D2D1_EXTEND_MODE,
    pub extendModeY: D2D1_EXTEND_MODE,
    pub interpolationMode: D2D1_INTERPOLATION_MODE,
}

struct SolidBrush {
    color: Cell<D2D1_COLOR_F>,
    opacity: Cell<f32>,
}

#[derive(Clone)]
pub struct ID2D1SolidColorBrush(Rc<SolidBrush>);

impl ID2D1SolidColorBrush {
    pub unsafe fn SetColor(&self, color: &D2D1_COLOR_F) {
        self.0.color.set(*color);
    }

    pub unsafe fn GetColor(&self) -> D2D1_COLOR_F {
        self.0.color.get()
    }

    pub unsafe fn SetOpacity(&self, opacity: f32) {
        self.0.opacity.set(opacity);
    }

    pub unsafe fn GetOpacity(&self) -> f32 {
        self.0.opacity.get()
    }
}

struct BitmapBrush {
    bitmap: ID2D1Bitmap,
    transform: Cell<Matrix3x2>,
    opacity: Cell<f32>,
    interpolation: Cell<D2D1_INTERPOLATION_MODE>,
}

/// A bitmap brush; extend modes are clamp, which the interface's cover transforms never reach.
#[derive(Clone)]
pub struct ID2D1BitmapBrush(Rc<BitmapBrush>);
pub type ID2D1BitmapBrush1 = ID2D1BitmapBrush;

impl ID2D1BitmapBrush {
    /// Maps the bitmap's DIPs into the space it paints in.
    pub unsafe fn SetTransform(&self, transform: &Matrix3x2) {
        self.0.transform.set(*transform);
    }

    pub unsafe fn GetTransform(&self, transform: &mut Matrix3x2) {
        *transform = self.0.transform.get();
    }

    pub unsafe fn SetOpacity(&self, opacity: f32) {
        self.0.opacity.set(opacity);
    }

    pub unsafe fn SetInterpolationMode1(&self, mode: D2D1_INTERPOLATION_MODE) {
        self.0.interpolation.set(mode);
    }
}

/// Any brush, as an opacity brush argument takes it.
#[derive(Clone)]
pub enum ID2D1Brush {
    Solid(ID2D1SolidColorBrush),
    Bitmap(ID2D1BitmapBrush),
}

impl From<ID2D1SolidColorBrush> for ID2D1Brush {
    fn from(b: ID2D1SolidColorBrush) -> Self {
        ID2D1Brush::Solid(b)
    }
}

impl From<ID2D1BitmapBrush> for ID2D1Brush {
    fn from(b: ID2D1BitmapBrush) -> Self {
        ID2D1Brush::Bitmap(b)
    }
}

impl Interface for ID2D1SolidColorBrush {}
impl Interface for ID2D1BitmapBrush {}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct D2D1_PIXEL_FORMAT {
    pub format: DXGI_FORMAT,
    pub alphaMode: D2D1_ALPHA_MODE,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_BITMAP_PROPERTIES {
    pub pixelFormat: D2D1_PIXEL_FORMAT,
    pub dpiX: f32,
    pub dpiY: f32,
}

struct Bitmap {
    /// BGRA rows, `width * 4` bytes each.
    data: CFRetained<CFData>,
    width: u32,
    height: u32,
    /// Bytes are premultiplied, or alpha is ignored.
    premultiplied: bool,
    dpi: (f32, f32),
    /// The image in the colour space it was last drawn in.
    image: RefCell<Option<(CFRetained<CGColorSpace>, CFRetained<CGImage>)>>,
}

/// A BGRA bitmap in memory, turned into a CGImage in the target's colour space when drawn.
#[derive(Clone)]
pub struct ID2D1Bitmap(Rc<Bitmap>);
pub type ID2D1Bitmap1 = ID2D1Bitmap;

impl Interface for ID2D1Bitmap {}

impl ID2D1Bitmap {
    /// Size in DIPs.
    pub unsafe fn GetSize(&self) -> D2D_SIZE_F {
        self.GetSize_()
    }

    pub(crate) fn GetSize_(&self) -> D2D_SIZE_F {
        let b = &self.0;
        D2D_SIZE_F {
            width: b.width as f32 * 96.0 / b.dpi.0,
            height: b.height as f32 * 96.0 / b.dpi.1,
        }
    }

    pub unsafe fn GetPixelSize(&self) -> D2D_SIZE_U {
        D2D_SIZE_U {
            width: self.0.width,
            height: self.0.height,
        }
    }

    pub(crate) fn image(&self, space: &CFRetained<CGColorSpace>) -> Option<CFRetained<CGImage>> {
        let b = &self.0;
        let mut slot = b.image.borrow_mut();
        if let Some((s, image)) = slot.as_ref()
            && CFRetained::as_ptr(s) == CFRetained::as_ptr(space)
        {
            return Some(image.clone());
        }
        let provider = CGDataProvider::with_cf_data(Some(&b.data))?;
        let alpha = if b.premultiplied {
            CGImageAlphaInfo::PremultipliedFirst
        } else {
            CGImageAlphaInfo::NoneSkipFirst
        };
        let info = CGBitmapInfo(CGImageByteOrderInfo::Order32Little.0 | alpha.0);
        let image = unsafe {
            CGImage::new(
                b.width as usize,
                b.height as usize,
                8,
                32,
                b.width as usize * 4,
                Some(space),
                info,
                Some(&provider),
                std::ptr::null(),
                true,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }?;
        *slot = Some((space.clone(), image.clone()));
        Some(image)
    }
}

/// Unused by the shim: layers need no resource object.
#[derive(Clone)]
pub struct ID2D1Layer;

#[derive(Default)]
pub struct D2D1_LAYER_PARAMETERS {
    pub contentBounds: D2D_RECT_F,
    pub geometricMask: std::mem::ManuallyDrop<Option<ID2D1Geometry>>,
    pub maskAntialiasMode: D2D1_ANTIALIAS_MODE,
    pub maskTransform: Matrix3x2,
    pub opacity: f32,
    pub opacityBrush: std::mem::ManuallyDrop<Option<ID2D1Brush>>,
    pub layerOptions: D2D1_LAYER_OPTIONS,
}

#[derive(Default)]
pub struct D2D1_LAYER_PARAMETERS1 {
    pub contentBounds: D2D_RECT_F,
    pub geometricMask: std::mem::ManuallyDrop<Option<ID2D1Geometry>>,
    pub maskAntialiasMode: D2D1_ANTIALIAS_MODE,
    pub maskTransform: Matrix3x2,
    pub opacity: f32,
    pub opacityBrush: std::mem::ManuallyDrop<Option<ID2D1Brush>>,
    pub layerOptions: D2D1_LAYER_OPTIONS1,
}

/// `D2D_MATRIX_4X4_F`, for DrawBitmap's perspective argument, which the shim ignores.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D_MATRIX_4X4_F {
    pub m: [[f32; 4]; 4],
}

/// The render target: what every draw goes through.
#[derive(Clone)]
pub struct ID2D1RenderTarget(pub(crate) Rc<RefCell<DcState>>);

/// `ID2D1DeviceContext`: the render target plus the device context's own overloads.
#[derive(Clone)]
pub struct ID2D1DeviceContext(ID2D1RenderTarget);

/// The interface's device context. Created once, then aimed at a bitmap each frame with
/// [`begin`](Self::begin) and [`end`](Self::end).
#[derive(Clone)]
pub struct ID2D1DeviceContext5(ID2D1DeviceContext);

impl Deref for ID2D1DeviceContext {
    type Target = ID2D1RenderTarget;
    fn deref(&self) -> &ID2D1RenderTarget {
        &self.0
    }
}

impl Deref for ID2D1DeviceContext5 {
    type Target = ID2D1DeviceContext;
    fn deref(&self) -> &ID2D1DeviceContext {
        &self.0
    }
}

macro_rules! views {
    ($($ty:ty),*) => {
        $(impl Interface for $ty {
            fn view(&self, want: std::any::TypeId) -> Option<Box<dyn Any>> {
                let rt: &ID2D1RenderTarget = self;
                let dc = ID2D1DeviceContext(rt.clone());
                if want == std::any::TypeId::of::<ID2D1RenderTarget>() {
                    Some(Box::new(rt.clone()))
                } else if want == std::any::TypeId::of::<ID2D1DeviceContext>() {
                    Some(Box::new(dc))
                } else if want == std::any::TypeId::of::<ID2D1DeviceContext5>() {
                    Some(Box::new(ID2D1DeviceContext5(dc)))
                } else {
                    None
                }
            }
        })*
    };
}

views!(ID2D1RenderTarget, ID2D1DeviceContext, ID2D1DeviceContext5);

impl Default for ID2D1DeviceContext5 {
    fn default() -> Self {
        Self::new()
    }
}

impl ID2D1DeviceContext5 {
    pub fn new() -> Self {
        let target = DcState {
            cg: None,
            space: srgb(),
            scale: 1.0,
            size: (0, 0),
            world: Matrix3x2::identity(),
            stack: Vec::new(),
            texts: HashMap::new(),
            frame: 0,
        };
        ID2D1DeviceContext5(ID2D1DeviceContext(ID2D1RenderTarget(Rc::new(
            RefCell::new(target),
        ))))
    }

    /// Starts drawing into `cg`, a premultiplied BGRA bitmap context of `size` pixels, with one
    /// DIP being `scale` pixels. Sets Direct2D's top-left origin and resets the transform; clears
    /// nothing.
    pub fn begin(&self, cg: &CGContext, size: (u32, u32), scale: f32) {
        let mut t = self.0.0.0.borrow_mut();
        if t.cg.is_some() {
            drop(t);
            self.end();
            t = self.0.0.0.borrow_mut();
        }
        let scale = if scale > 0.0 { scale } else { 1.0 };
        CGContext::save_g_state(Some(cg));
        CGContext::translate_ctm(Some(cg), 0.0, f64::from(size.1));
        CGContext::scale_ctm(Some(cg), scale.into(), (-scale).into());
        // Grayscale antialiasing, glyphs at fractional positions, like DirectWrite's
        // natural mode with grayscale text.
        CGContext::set_should_antialias(Some(cg), true);
        CGContext::set_allows_font_smoothing(Some(cg), true);
        CGContext::set_should_smooth_fonts(Some(cg), false);
        CGContext::set_allows_font_subpixel_positioning(Some(cg), true);
        CGContext::set_should_subpixel_position_fonts(Some(cg), true);
        CGContext::set_interpolation_quality(Some(cg), CGInterpolationQuality::Medium);
        if let Some(space) = CGBitmapContextGetColorSpace(Some(cg)) {
            t.space = space;
        }
        t.cg = Some(CFRetained::from(cg));
        t.size = size;
        t.scale = scale;
        t.world = Matrix3x2::identity();
        t.frame += 1;
    }

    /// Ends the frame: pops whatever is still pushed and gives the context back as it came.
    pub fn end(&self) {
        let mut t = self.0.0.0.borrow_mut();
        let Some(cg) = t.cg.take() else { return };
        for pushed in std::mem::take(&mut t.stack).into_iter().rev() {
            if let Pushed::Layer { transparency: true } = pushed {
                CGContext::end_transparency_layer(Some(&cg));
            }
            CGContext::restore_g_state(Some(&cg));
        }
        CGContext::restore_g_state(Some(&cg));
        // Layouts no frame drew lately go.
        let frame = t.frame;
        t.texts.retain(|_, (_, used)| frame - *used < 3);
        t.world = Matrix3x2::identity();
    }

    pub unsafe fn DrawSvgDocument(&self, document: &ID2D1SvgDocument) {
        let t = self.0.0.0.borrow();
        document.draw(&t);
    }

    /// Parses an SVG; `stream` is its bytes, in place of an IStream.
    pub unsafe fn CreateSvgDocument(
        &self,
        stream: &[u8],
        viewportsize: D2D_SIZE_F,
    ) -> Result<ID2D1SvgDocument> {
        ID2D1SvgDocument::parse(stream, viewportsize)
    }
}

impl ID2D1DeviceContext {
    pub unsafe fn PushLayer(&self, params: &D2D1_LAYER_PARAMETERS1, _layer: Option<&ID2D1Layer>) {
        self.0.push_layer(
            &params.contentBounds,
            params.geometricMask.as_ref(),
            params.maskAntialiasMode,
            &params.maskTransform,
            params.opacity,
        );
    }

    pub unsafe fn DrawBitmap(
        &self,
        bitmap: &ID2D1Bitmap,
        destinationrectangle: Option<&D2D_RECT_F>,
        opacity: f32,
        interpolationmode: D2D1_INTERPOLATION_MODE,
        sourcerectangle: Option<&D2D_RECT_F>,
        _perspectivetransform: Option<&D2D_MATRIX_4X4_F>,
    ) {
        self.0.draw_bitmap(
            bitmap,
            destinationrectangle,
            opacity,
            interpolationmode,
            sourcerectangle,
        );
    }

    pub unsafe fn CreateBitmapBrush(
        &self,
        bitmap: &ID2D1Bitmap,
        bitmapbrushproperties: Option<&D2D1_BITMAP_BRUSH_PROPERTIES1>,
        brushproperties: Option<&D2D1_BRUSH_PROPERTIES>,
    ) -> Result<ID2D1BitmapBrush1> {
        let interpolation =
            bitmapbrushproperties.map_or(D2D1_INTERPOLATION_MODE_LINEAR, |p| p.interpolationMode);
        Ok(ID2D1BitmapBrush(Rc::new(BitmapBrush {
            bitmap: bitmap.clone(),
            transform: Cell::new(brushproperties.map_or(Matrix3x2::identity(), |p| p.transform)),
            opacity: Cell::new(brushproperties.map_or(1.0, |p| p.opacity)),
            interpolation: Cell::new(interpolation),
        })))
    }
}

impl ID2D1RenderTarget {
    fn t(&self) -> std::cell::Ref<'_, DcState> {
        self.0.borrow()
    }

    pub unsafe fn BeginDraw(&self) {}

    /// Ends nothing by itself: [`ID2D1DeviceContext5::end`] finishes the frame.
    pub unsafe fn EndDraw(&self, _tag1: Option<&mut u64>, _tag2: Option<&mut u64>) -> Result<()> {
        Ok(())
    }

    pub unsafe fn SetTransform(&self, transform: &Matrix3x2) {
        self.0.borrow_mut().world = *transform;
    }

    pub unsafe fn GetTransform(&self, transform: &mut Matrix3x2) {
        *transform = self.t().world;
    }

    pub unsafe fn SetDpi(&self, dpix: f32, _dpiy: f32) {
        self.0.borrow_mut().scale = dpix / 96.0;
    }

    pub unsafe fn GetDpi(&self, dpix: &mut f32, dpiy: &mut f32) {
        let s = self.t().scale * 96.0;
        (*dpix, *dpiy) = (s, s);
    }

    /// Size in DIPs.
    pub unsafe fn GetSize(&self) -> D2D_SIZE_F {
        let t = self.t();
        D2D_SIZE_F {
            width: t.size.0 as f32 / t.scale,
            height: t.size.1 as f32 / t.scale,
        }
    }

    pub unsafe fn GetPixelSize(&self) -> D2D_SIZE_U {
        let t = self.t();
        D2D_SIZE_U {
            width: t.size.0,
            height: t.size.1,
        }
    }

    pub unsafe fn GetMaximumBitmapSize(&self) -> u32 {
        16384
    }

    /// Core Graphics' antialiasing is always on; grayscale text is all it draws.
    pub unsafe fn SetAntialiasMode(&self, _mode: D2D1_ANTIALIAS_MODE) {}

    pub unsafe fn SetTextAntialiasMode(&self, _mode: D2D1_TEXT_ANTIALIAS_MODE) {}

    /// Sets every pixel inside the clips to `color`, transparent black for None.
    pub unsafe fn Clear(&self, color: Option<&D2D1_COLOR_F>) {
        let t = self.t();
        let Some(cg) = t.cg() else { return };
        let c = color.copied().unwrap_or_default();
        CGContext::save_g_state(Some(cg));
        CGContext::set_blend_mode(Some(cg), CGBlendMode::Copy);
        t.set_fill(cg, &c, 1.0);
        let all = D2D_RECT_F {
            left: f32::MIN,
            top: f32::MIN,
            right: f32::MAX,
            bottom: f32::MAX,
        };
        CGContext::fill_rect(Some(cg), all.cg());
        CGContext::restore_g_state(Some(cg));
    }

    pub unsafe fn CreateSolidColorBrush(
        &self,
        color: &D2D1_COLOR_F,
        brushproperties: Option<&D2D1_BRUSH_PROPERTIES>,
    ) -> Result<ID2D1SolidColorBrush> {
        Ok(ID2D1SolidColorBrush(Rc::new(SolidBrush {
            color: Cell::new(*color),
            opacity: Cell::new(brushproperties.map_or(1.0, |p| p.opacity)),
        })))
    }

    /// Copies BGRA rows into a bitmap. Formats other than B8G8R8A8 are refused.
    pub unsafe fn CreateBitmap(
        &self,
        size: D2D_SIZE_U,
        srcdata: Option<*const c_void>,
        pitch: u32,
        bitmapproperties: &D2D1_BITMAP_PROPERTIES,
    ) -> Result<ID2D1Bitmap> {
        let p = bitmapproperties;
        if !matches!(
            p.pixelFormat.format,
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_UNKNOWN
        ) || size.width == 0
            || size.height == 0
        {
            return Err(Error::new(E_INVALIDARG, "unsupported bitmap"));
        }
        let row = size.width as usize * 4;
        let mut bytes = vec![0u8; row * size.height as usize];
        if let Some(src) = srcdata.filter(|s| !s.is_null()) {
            for y in 0..size.height as usize {
                let from = unsafe { (src as *const u8).add(y * pitch as usize) };
                let to = &mut bytes[y * row..(y + 1) * row];
                unsafe { std::ptr::copy_nonoverlapping(from, to.as_mut_ptr(), row) };
            }
        }
        let dpi = |d: f32| if d > 0.0 { d } else { 96.0 };
        Ok(ID2D1Bitmap(Rc::new(Bitmap {
            data: CFData::from_bytes(&bytes),
            width: size.width,
            height: size.height,
            premultiplied: p.pixelFormat.alphaMode != D2D1_ALPHA_MODE_IGNORE,
            dpi: (dpi(p.dpiX), dpi(p.dpiY)),
            image: RefCell::new(None),
        })))
    }

    pub unsafe fn CreateBitmapBrush(
        &self,
        bitmap: &ID2D1Bitmap,
        bitmapbrushproperties: Option<&D2D1_BITMAP_BRUSH_PROPERTIES1>,
        brushproperties: Option<&D2D1_BRUSH_PROPERTIES>,
    ) -> Result<ID2D1BitmapBrush> {
        unsafe {
            ID2D1DeviceContext(self.clone()).CreateBitmapBrush(
                bitmap,
                bitmapbrushproperties,
                brushproperties,
            )
        }
    }

    pub unsafe fn FillRectangle(&self, rect: &D2D_RECT_F, brush: impl BrushParam) {
        let r = rect.normalized();
        if r.right <= r.left || r.bottom <= r.top {
            return;
        }
        let path = unsafe { CGPath::with_rect(r.cg(), std::ptr::null()) };
        self.t().fill(&path, false, &brush.paint());
    }

    pub unsafe fn DrawRectangle(
        &self,
        rect: &D2D_RECT_F,
        brush: impl BrushParam,
        strokewidth: f32,
        strokestyle: impl StrokeParam,
    ) {
        let path = unsafe { CGPath::with_rect(rect.cg(), std::ptr::null()) };
        self.t()
            .stroke(&path, strokewidth, strokestyle.style(), &brush.paint());
    }

    pub unsafe fn FillRoundedRectangle(
        &self,
        roundedrect: &D2D1_ROUNDED_RECT,
        brush: impl BrushParam,
    ) {
        let Some(path) = rounded_path(roundedrect) else {
            return;
        };
        self.t().fill(&path, false, &brush.paint());
    }

    pub unsafe fn DrawRoundedRectangle(
        &self,
        roundedrect: &D2D1_ROUNDED_RECT,
        brush: impl BrushParam,
        strokewidth: f32,
        strokestyle: impl StrokeParam,
    ) {
        let Some(path) = rounded_path(roundedrect) else {
            return;
        };
        self.t()
            .stroke(&path, strokewidth, strokestyle.style(), &brush.paint());
    }

    pub unsafe fn FillEllipse(&self, ellipse: &D2D1_ELLIPSE, brush: impl BrushParam) {
        let path = ellipse_path(ellipse);
        self.t().fill(&path, false, &brush.paint());
    }

    pub unsafe fn DrawEllipse(
        &self,
        ellipse: &D2D1_ELLIPSE,
        brush: impl BrushParam,
        strokewidth: f32,
        strokestyle: impl StrokeParam,
    ) {
        let path = ellipse_path(ellipse);
        self.t()
            .stroke(&path, strokewidth, strokestyle.style(), &brush.paint());
    }

    pub unsafe fn DrawLine(
        &self,
        point0: Vector2,
        point1: Vector2,
        brush: impl BrushParam,
        strokewidth: f32,
        strokestyle: impl StrokeParam,
    ) {
        let path = CGMutablePath::new();
        unsafe {
            CGMutablePath::move_to_point(
                Some(&path),
                std::ptr::null(),
                point0.X.into(),
                point0.Y.into(),
            );
            CGMutablePath::add_line_to_point(
                Some(&path),
                std::ptr::null(),
                point1.X.into(),
                point1.Y.into(),
            );
        }
        self.t()
            .stroke(&path, strokewidth, strokestyle.style(), &brush.paint());
    }

    pub unsafe fn FillGeometry(
        &self,
        geometry: &ID2D1Geometry,
        brush: impl BrushParam,
        _opacitybrush: Option<&ID2D1Brush>,
    ) {
        if let Some((path, even_odd)) = geometry.fill_path() {
            self.t().fill(&path, even_odd, &brush.paint());
        }
    }

    pub unsafe fn DrawGeometry(
        &self,
        geometry: &ID2D1Geometry,
        brush: impl BrushParam,
        strokewidth: f32,
        strokestyle: impl StrokeParam,
    ) {
        if let Some(path) = geometry.stroke_path() {
            self.t()
                .stroke(&path, strokewidth, strokestyle.style(), &brush.paint());
        }
    }

    /// Clips to `rect` under the current transform (its bounding box when that rotates),
    /// snapped to whole pixels when aliased, until PopAxisAlignedClip.
    pub unsafe fn PushAxisAlignedClip(&self, rect: &D2D_RECT_F, mode: D2D1_ANTIALIAS_MODE) {
        let mut t = self.0.borrow_mut();
        let Some(cg) = t.cg.clone() else { return };
        let (mut l, mut tp, mut r, mut b) = t.device_rect(rect);
        if mode == D2D1_ANTIALIAS_MODE_ALIASED {
            // Pixels whose centres are inside.
            (l, tp, r, b) = (l.round(), tp.round(), r.round(), b.round());
        }
        CGContext::save_g_state(Some(&cg));
        t.clip_device((l, tp, r, b));
        t.stack.push(Pushed::Clip);
    }

    pub unsafe fn PopAxisAlignedClip(&self) {
        self.pop();
    }

    pub unsafe fn PushLayer(&self, params: &D2D1_LAYER_PARAMETERS, _layer: Option<&ID2D1Layer>) {
        self.push_layer(
            &params.contentBounds,
            params.geometricMask.as_ref(),
            params.maskAntialiasMode,
            &params.maskTransform,
            params.opacity,
        );
    }

    pub unsafe fn PopLayer(&self) {
        self.pop();
    }

    fn push_layer(
        &self,
        bounds: &D2D_RECT_F,
        mask: Option<&ID2D1Geometry>,
        mask_mode: D2D1_ANTIALIAS_MODE,
        mask_transform: &Matrix3x2,
        opacity: f32,
    ) {
        let mut t = self.0.borrow_mut();
        let Some(cg) = t.cg.clone() else { return };
        CGContext::save_g_state(Some(&cg));
        // Content bounds, rounded outwards to whole pixels.
        let (l, tp, r, b) = t.device_rect(bounds);
        t.clip_device((l.floor(), tp.floor(), r.ceil(), b.ceil()));
        if let Some((path, even_odd)) = mask.and_then(|m| m.fill_path()) {
            // The CTM is DIPs here; the mask goes through its own transform, then the world's.
            let m = (*mask_transform * t.world).cg();
            let placed = unsafe { CGPath::new_copy_by_transforming_path(Some(&path), &m) };
            let aliased = mask_mode == D2D1_ANTIALIAS_MODE_ALIASED;
            CGContext::set_should_antialias(Some(&cg), !aliased);
            CGContext::add_path(Some(&cg), placed.as_deref());
            if even_odd {
                CGContext::eo_clip(Some(&cg));
            } else {
                CGContext::clip(Some(&cg));
            }
            CGContext::set_should_antialias(Some(&cg), true);
        }
        let transparency = opacity < 1.0;
        if transparency {
            CGContext::set_alpha(Some(&cg), opacity.max(0.0).into());
            unsafe { CGContext::begin_transparency_layer(Some(&cg), None) };
        }
        t.stack.push(Pushed::Layer { transparency });
    }

    fn pop(&self) {
        let mut t = self.0.borrow_mut();
        let Some(cg) = t.cg.clone() else { return };
        match t.stack.pop() {
            Some(Pushed::Layer { transparency: true }) => {
                CGContext::end_transparency_layer(Some(&cg));
                CGContext::restore_g_state(Some(&cg));
            }
            Some(_) => CGContext::restore_g_state(Some(&cg)),
            None => {}
        }
    }

    pub unsafe fn DrawBitmap(
        &self,
        bitmap: &ID2D1Bitmap,
        destinationrectangle: Option<&D2D_RECT_F>,
        opacity: f32,
        interpolationmode: D2D1_BITMAP_INTERPOLATION_MODE,
        sourcerectangle: Option<&D2D_RECT_F>,
    ) {
        let mode = match interpolationmode {
            D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR => {
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
            }
            _ => D2D1_INTERPOLATION_MODE_LINEAR,
        };
        self.draw_bitmap(bitmap, destinationrectangle, opacity, mode, sourcerectangle);
    }

    fn draw_bitmap(
        &self,
        bitmap: &ID2D1Bitmap,
        dest: Option<&D2D_RECT_F>,
        opacity: f32,
        mode: D2D1_INTERPOLATION_MODE,
        source: Option<&D2D_RECT_F>,
    ) {
        let t = self.t();
        let size = bitmap.GetSize_();
        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: size.width,
            bottom: size.height,
        };
        let src = source.copied().unwrap_or(full).normalized();
        let dst = dest.copied().unwrap_or(src).normalized();
        if src.right <= src.left || src.bottom <= src.top || dst.right <= dst.left {
            return;
        }
        let Some(image) = bitmap.image(&t.space) else {
            return;
        };
        t.with_world(|cg| {
            CGContext::clip_to_rect(Some(cg), dst.cg());
            CGContext::set_alpha(Some(cg), opacity.clamp(0.0, 1.0).into());
            CGContext::set_interpolation_quality(Some(cg), quality(mode));
            // The whole image placed so that `src` lands on `dst`.
            let sx = (dst.right - dst.left) / (src.right - src.left);
            let sy = (dst.bottom - dst.top) / (src.bottom - src.top);
            let x = dst.left - src.left * sx;
            let y = dst.top - src.top * sy;
            draw_image(cg, &image, (x, y, size.width * sx, size.height * sy));
        });
    }

    /// Lays `string` out in `layoutrect` and draws it, as DrawText does.
    pub unsafe fn DrawText(
        &self,
        string: &[u16],
        textformat: &IDWriteTextFormat,
        layoutrect: &D2D_RECT_F,
        defaultfillbrush: impl BrushParam,
        options: D2D1_DRAW_TEXT_OPTIONS,
        _measuringmode: DWRITE_MEASURING_MODE,
    ) {
        let r = layoutrect;
        let (w, h) = ((r.right - r.left).max(0.0), (r.bottom - r.top).max(0.0));
        let key = TextKey {
            format: textformat.id(),
            generation: textformat.generation(),
            text: string.to_vec(),
            width: w.to_bits(),
            height: h.to_bits(),
        };
        let layout = {
            let mut t = self.0.borrow_mut();
            let frame = t.frame;
            if let Some((layout, used)) = t.texts.get_mut(&key) {
                *used = frame;
                layout.clone()
            } else {
                let layout = IDWriteTextLayout::new(string, textformat, w, h);
                t.texts.insert(key, (layout.clone(), frame));
                layout
            }
        };
        let origin = Vector2 {
            X: r.left,
            Y: r.top,
        };
        layout.draw(&self.t(), origin, &defaultfillbrush.paint(), options);
    }

    pub unsafe fn DrawTextLayout(
        &self,
        origin: Vector2,
        textlayout: &IDWriteTextLayout,
        defaultfillbrush: impl BrushParam,
        options: D2D1_DRAW_TEXT_OPTIONS,
    ) {
        textlayout.draw(&self.t(), origin, &defaultfillbrush.paint(), options);
    }
}

/// A rounded rectangle's path; the radii are clamped to half the sides, as Direct2D does.
pub(crate) fn rounded_path(rr: &D2D1_ROUNDED_RECT) -> Option<CFRetained<CGPath>> {
    let r = rr.rect.normalized();
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let rx = rr.radiusX.clamp(0.0, w / 2.0);
    let ry = rr.radiusY.clamp(0.0, h / 2.0);
    Some(unsafe { CGPath::with_rounded_rect(r.cg(), rx.into(), ry.into(), std::ptr::null()) })
}

pub(crate) fn ellipse_path(e: &D2D1_ELLIPSE) -> CFRetained<CGPath> {
    let rect = D2D_RECT_F {
        left: e.point.X - e.radiusX,
        top: e.point.Y - e.radiusY,
        right: e.point.X + e.radiusX,
        bottom: e.point.Y + e.radiusY,
    };
    unsafe { CGPath::with_ellipse_in_rect(rect.cg(), std::ptr::null()) }
}

/// The resource factory. Geometries and stroke styles need no device.
#[derive(Clone, Default)]
pub struct ID2D1Factory1(());
pub type ID2D1Factory = ID2D1Factory1;

impl Interface for ID2D1Factory1 {}

impl ID2D1Factory1 {
    pub fn new() -> Self {
        ID2D1Factory1(())
    }

    pub unsafe fn CreateStrokeStyle(
        &self,
        props: &D2D1_STROKE_STYLE_PROPERTIES1,
        dashes: Option<&[f32]>,
    ) -> Result<ID2D1StrokeStyle1> {
        Ok(ID2D1StrokeStyle1(ID2D1StrokeStyle(Rc::new(StrokeStyle {
            props: *props,
            dashes: dashes.map(<[f32]>::to_vec),
        }))))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_STROKE_STYLE_PROPERTIES1 {
    pub startCap: D2D1_CAP_STYLE,
    pub endCap: D2D1_CAP_STYLE,
    pub dashCap: D2D1_CAP_STYLE,
    pub lineJoin: D2D1_LINE_JOIN,
    pub miterLimit: f32,
    pub dashStyle: D2D1_DASH_STYLE,
    pub dashOffset: f32,
    pub transformType: D2D1_STROKE_TRANSFORM_TYPE,
}

pub(crate) struct StrokeStyle {
    pub(crate) props: D2D1_STROKE_STYLE_PROPERTIES1,
    dashes: Option<Vec<f32>>,
}

#[derive(Clone)]
pub struct ID2D1StrokeStyle(pub(crate) Rc<StrokeStyle>);
/// What the factory makes; `.into()` gives the `ID2D1StrokeStyle` draws take.
#[derive(Clone)]
pub struct ID2D1StrokeStyle1(ID2D1StrokeStyle);

impl Deref for ID2D1StrokeStyle1 {
    type Target = ID2D1StrokeStyle;
    fn deref(&self) -> &ID2D1StrokeStyle {
        &self.0
    }
}

impl From<ID2D1StrokeStyle1> for ID2D1StrokeStyle {
    fn from(s: ID2D1StrokeStyle1) -> Self {
        s.0
    }
}

impl Interface for ID2D1StrokeStyle {}

impl ID2D1StrokeStyle {
    /// Dash lengths in stroke widths, for the style's dash style.
    fn dashes(&self) -> Option<Vec<f32>> {
        match self.0.props.dashStyle {
            D2D1_DASH_STYLE_CUSTOM => self.0.dashes.clone().filter(|d| !d.is_empty()),
            D2D1_DASH_STYLE_DASH => Some(vec![2.0, 2.0]),
            D2D1_DASH_STYLE_DOT => Some(vec![0.0, 2.0]),
            D2D1_DASH_STYLE_DASH_DOT => Some(vec![2.0, 2.0, 0.0, 2.0]),
            D2D1_DASH_STYLE_DASH_DOT_DOT => Some(vec![2.0, 2.0, 0.0, 2.0, 0.0, 2.0]),
            _ => None,
        }
    }
}

/// For tests and tools: a premultiplied BGRA bitmap context with its pixels.
pub struct Canvas {
    pub cg: CFRetained<CGContext>,
    pub width: u32,
    pub height: u32,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Option<Canvas> {
        let space = srgb();
        let info = CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0;
        let cg = unsafe {
            objc2_core_graphics::CGBitmapContextCreate(
                std::ptr::null_mut(),
                width as usize,
                height as usize,
                8,
                width as usize * 4,
                Some(&space),
                info,
            )
        }?;
        Some(Canvas { cg, width, height })
    }

    /// BGRA premultiplied rows, top row first.
    pub fn pixels(&self) -> Vec<u8> {
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(&self.cg));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(&self.cg));
        let Some(data) = NonNull::new(data.cast::<u8>()) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        for y in 0..self.height as usize {
            let row = unsafe {
                std::slice::from_raw_parts(data.as_ptr().add(y * stride), self.width as usize * 4)
            };
            out.extend_from_slice(row);
        }
        out
    }

    /// Writes the pixels as a PNG, un-premultiplied.
    pub fn save_png(&self, path: &std::path::Path) -> std::io::Result<()> {
        let bgra = self.pixels();
        let rgba: Vec<u8> = bgra
            .chunks(4)
            .flat_map(|p| {
                let a = p[3];
                let un = |c: u8| {
                    if a == 0 {
                        0
                    } else {
                        ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
                    }
                };
                [un(p[2]), un(p[1]), un(p[0]), a]
            })
            .collect();
        image::save_buffer(
            path,
            &rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(std::io::Error::other)
    }
}
