//! SVG documents: parsed once with usvg, rasterised with resvg for each device transform they
//! are drawn at, and cached, so icons stay as sharp as Direct2D's vector rendering.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality,
};

use super::*;

/// Rasters kept per document; icons are drawn at a handful of positions and scales.
const MAX_CACHED: usize = 64;

/// A raster of the document under one device transform, placed at whole pixels.
struct Raster {
    image: CFRetained<CGImage>,
    space: CFRetained<CGColorSpace>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// The device transform's linear part (1/1024ths) and its translation's fraction (1/16 px).
type Key = (i32, i32, i32, i32, i32, i32);

struct Document {
    tree: resvg::usvg::Tree,
    viewport: D2D_SIZE_F,
    rasters: RefCell<HashMap<Key, Rc<Raster>>>,
}

#[derive(Clone)]
pub struct ID2D1SvgDocument(Rc<Document>);

impl Interface for ID2D1SvgDocument {}

impl ID2D1SvgDocument {
    pub(crate) fn parse(bytes: &[u8], viewport: D2D_SIZE_F) -> Result<Self> {
        let options = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_data(bytes, &options)
            .map_err(|e| Error::new(E_INVALIDARG, format!("svg: {e}")))?;
        Ok(ID2D1SvgDocument(Rc::new(Document {
            tree,
            viewport,
            rasters: RefCell::new(HashMap::new()),
        })))
    }

    pub unsafe fn GetViewportSize(&self) -> D2D_SIZE_F {
        self.0.viewport
    }

    /// Draws the document's viewport at the target's transform, as DrawSvgDocument does.
    pub(crate) fn draw(&self, t: &DcState) {
        let Some(cg) = t.cg() else { return };
        // World DIPs to device pixels, y down.
        let d = t.world * Matrix3x2::scale(t.scale, t.scale);
        let Some(raster) = self.raster(&d, &t.space) else {
            return;
        };
        let s = t.scale;
        CGContext::save_g_state(Some(cg));
        CGContext::set_interpolation_quality(Some(cg), CGInterpolationQuality::None);
        let rect = (
            raster.x as f32 / s,
            raster.y as f32 / s,
            raster.width as f32 / s,
            raster.height as f32 / s,
        );
        draw_image(cg, &raster.image, rect);
        CGContext::restore_g_state(Some(cg));
    }

    fn raster(&self, d: &Matrix3x2, space: &CFRetained<CGColorSpace>) -> Option<Rc<Raster>> {
        let doc = &self.0;
        let size = doc.tree.size();
        let (vw, vh) = (doc.viewport.width, doc.viewport.height);
        // The viewport stretches the document's own size, as Direct2D's does.
        let (sx, sy) = (vw / size.width().max(1e-6), vh / size.height().max(1e-6));
        let m = Matrix3x2::scale(sx, sy) * *d;
        let q = |v: f32, by: f32| (v * by).round() as i32;
        let (fx, fy) = (m.M31 - m.M31.floor(), m.M32 - m.M32.floor());
        let key = (
            q(m.M11, 1024.0),
            q(m.M12, 1024.0),
            q(m.M21, 1024.0),
            q(m.M22, 1024.0),
            q(fx, 16.0),
            q(fy, 16.0),
        );
        let (bx, by) = (m.M31.floor() as i32, m.M32.floor() as i32);
        if let Some(r) = doc.rasters.borrow().get(&key)
            && CFRetained::as_ptr(&r.space) == CFRetained::as_ptr(space)
        {
            return Some(Rc::new(Raster {
                image: r.image.clone(),
                space: r.space.clone(),
                x: r.x + bx,
                y: r.y + by,
                width: r.width,
                height: r.height,
            }));
        }
        // Rasterise with the translation's fraction, then place at whole pixels.
        let mut local = m;
        local.M31 = key.4 as f32 / 16.0;
        local.M32 = key.5 as f32 / 16.0;
        let (w, h) = (size.width(), size.height());
        let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| local.transform(x, y));
        let x0 = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min).floor();
        let y0 = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min).floor();
        let x1 = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max).ceil();
        let y1 = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max).ceil();
        let (pw, ph) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if pw == 0 || ph == 0 || pw > 8192 || ph > 8192 {
            return None;
        }
        let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
        let transform = resvg::tiny_skia::Transform::from_row(
            local.M11,
            local.M12,
            local.M21,
            local.M22,
            local.M31 - x0,
            local.M32 - y0,
        );
        resvg::render(&doc.tree, transform, &mut pixmap.as_mut());
        // tiny-skia's pixels are premultiplied RGBA, which CGImage takes as they are.
        let data = CFData::from_bytes(pixmap.data());
        let provider = CGDataProvider::with_cf_data(Some(&data))?;
        let info = CGBitmapInfo(
            CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
        );
        let image = unsafe {
            CGImage::new(
                pw as usize,
                ph as usize,
                8,
                32,
                pw as usize * 4,
                Some(space),
                info,
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }?;
        let cached = Rc::new(Raster {
            image,
            space: space.clone(),
            x: x0 as i32,
            y: y0 as i32,
            width: pw,
            height: ph,
        });
        let mut rasters = doc.rasters.borrow_mut();
        if rasters.len() >= MAX_CACHED {
            rasters.clear();
        }
        rasters.insert(key, cached.clone());
        Some(Rc::new(Raster {
            image: cached.image.clone(),
            space: space.clone(),
            x: cached.x + bx,
            y: cached.y + by,
            width: pw,
            height: ph,
        }))
    }
}
