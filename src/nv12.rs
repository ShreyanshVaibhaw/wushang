//! Software frames repacked into NV12 for the GPU's video processor, which converts them to RGB
//! and applies the colour filters as for hardware frames (drivers skip those filters on RGB
//! input). The decoder thread does it, off the render thread: a 4K frame takes several
//! milliseconds even split across every core.
//!
//! ponytail: 10-bit video is repacked to 8-bit too, the slowest swscale path and a loss of
//! precision; upload P010 textures for 10-bit sources if 4K60 10-bit software playback matters.

use ff::ffi;
use ff::format::Pixel;
use ffmpeg_next as ff;

/// Converts frames to NV12, keeping a swscale context for the current format and size.
#[derive(Default)]
pub struct Nv12 {
    scaler: Option<((Pixel, u32, u32), Scaler)>,
}

impl Nv12 {
    /// `frame` as NV12 with its timestamps, aspect ratio and colour tags. YUV keeps its matrix
    /// and range, so only the planes move; RGB becomes BT.709 studio range and is tagged so.
    pub fn convert(&mut self, frame: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
        if frame.format() == Pixel::NV12 {
            return Ok(frame.clone());
        }
        let key = (frame.format(), frame.width(), frame.height());
        if self.scaler.as_ref().is_none_or(|(k, _)| *k != key) {
            self.scaler = Some((key, Scaler::new(frame)?));
        }
        let (_, scaler) = self.scaler.as_mut().unwrap();
        let mut out = ff::frame::Video::new(Pixel::NV12, frame.width(), frame.height());
        unsafe {
            let err = ffi::sws_scale_frame(scaler.0, out.as_mut_ptr(), frame.as_ptr());
            if err < 0 {
                return Err(ff::Error::from(err));
            }
            ffi::av_frame_copy_props(out.as_mut_ptr(), frame.as_ptr());
        }
        if is_rgb(frame.format()) {
            out.set_color_space(ff::color::Space::BT709);
            out.set_color_range(ff::color::Range::MPEG);
        }
        Ok(out)
    }
}

/// A swscale context for one format and size into NV12, splitting each frame across every
/// core.
struct Scaler(*mut ffi::SwsContext);

// Only the decoder thread that made it uses it.
unsafe impl Send for Scaler {}

impl Scaler {
    fn new(frame: &ff::frame::Video) -> Result<Self, ff::Error> {
        let (w, h) = (frame.width() as i32, frame.height() as i32);
        unsafe {
            let ctx = ffi::sws_alloc_context();
            if ctx.is_null() {
                return Err(ff::Error::Bug);
            }
            let scaler = Scaler(ctx);
            (*ctx).flags = ffi::SwsFlags::SWS_BILINEAR as u32;
            ((*ctx).src_w, (*ctx).src_h, (*ctx).dst_w, (*ctx).dst_h) = (w, h, w, h);
            (*ctx).src_format = ffi::AVPixelFormat::from(frame.format()) as i32;
            (*ctx).dst_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            // 0 picks one slice thread per core.
            (*ctx).threads = 0;
            let err = ffi::sws_init_context(ctx, std::ptr::null_mut(), std::ptr::null_mut());
            if err < 0 {
                return Err(ff::Error::from(err));
            }
            let rgb = is_rgb(frame.format());
            let full = !rgb && frame.color_range() == ff::color::Range::JPEG;
            let matrix = if rgb || crate::render::is_bt709(frame) {
                ffi::SWS_CS_ITU709
            } else {
                ffi::SWS_CS_DEFAULT
            };
            let coefficients = ffi::sws_getCoefficients(matrix);
            ffi::sws_setColorspaceDetails(
                ctx,
                coefficients,
                i32::from(rgb || full),
                coefficients,
                i32::from(full),
                0,
                1 << 16,
                1 << 16,
            );
            Ok(scaler)
        }
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.0) }
    }
}

fn is_rgb(format: Pixel) -> bool {
    unsafe {
        let desc = ffi::av_pix_fmt_desc_get(format.into());
        !desc.is_null() && (*desc).flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repacks_and_keeps_the_frame_tags() {
        let mut src = ff::frame::Video::new(Pixel::YUV420P10LE, 64, 36);
        src.set_pts(Some(42));
        src.set_color_range(ff::color::Range::JPEG);
        let out = Nv12::default().convert(&src).unwrap();
        assert_eq!(
            (out.format(), out.width(), out.height()),
            (Pixel::NV12, 64, 36)
        );
        assert_eq!(
            (out.pts(), out.color_range()),
            (Some(42), ff::color::Range::JPEG)
        );

        let rgb = ff::frame::Video::new(Pixel::RGB24, 64, 36);
        let out = Nv12::default().convert(&rgb).unwrap();
        assert_eq!(out.color_space(), ff::color::Space::BT709);
        assert_eq!(out.color_range(), ff::color::Range::MPEG);
    }
}
