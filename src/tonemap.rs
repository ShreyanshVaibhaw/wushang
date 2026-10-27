//! HDR10 video brought to the SDR back buffer with Direct2D effects. The video processor only
//! turns a frame from YCbCr into RGB, still PQ-encoded in BT.2020, which every driver that
//! supports HDR displays can do. Direct2D does the rest:
//!
//! 1. PQ to linear scRGB: BT.709 primaries, 1.0 is 80 nits, BT.2020 colours outside 0..1.
//! 2. Scaled so the stream's peak is 1.0, the domain of the next step's table.
//! 3. A tone curve (`curve`) per channel: unchanged up to most of HDR's reference white (203 nits,
//!    BT.2408), highlights above rolled off into the rest of SDR's range.
//! 4. Linear to sRGB.
//!
//! Direct2D's own HDR Tone Map effect was tried first; its curve darkened even mid-greys by a third.

use std::cell::Cell;

use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, Result};

/// SDR white in scRGB, and where HDR video puts its diffuse white (BT.2408).
const SDR_WHITE: f32 = 80.0;
const HDR_WHITE: f32 = 203.0;
/// Light up to this share of reference white passes unchanged.
const KNEE: f32 = 0.75;
/// Table entries; Direct2D interpolates between them.
const TABLE: usize = 1024;

pub struct ToneMap {
    linear: ID2D1Effect,
    scale: ID2D1Effect,
    curve: ID2D1Effect,
    srgb: ID2D1Effect,
    /// The peak the scale and curve are set for.
    peak: Cell<f32>,
}

impl ToneMap {
    pub fn new(ctx: &ID2D1DeviceContext5) -> Result<Self> {
        unsafe {
            let linear = ctx.CreateEffect(&CLSID_D2D1ColorManagement)?;
            let scale = ctx.CreateEffect(&CLSID_D2D1WhiteLevelAdjustment)?;
            let curve = ctx.CreateEffect(&CLSID_D2D1TableTransfer)?;
            let srgb = ctx.CreateEffect(&CLSID_D2D1ColorManagement)?;
            let space = |s| ctx.CreateColorContextFromDxgiColorSpace(s);
            let hdr10 = space(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)?;
            let scrgb = space(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
            let display = space(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)?;
            for (effect, from, to) in [(&linear, &hdr10, &scrgb), (&srgb, &scrgb, &display)] {
                set_context(
                    effect,
                    D2D1_COLORMANAGEMENT_PROP_SOURCE_COLOR_CONTEXT.0,
                    from,
                )?;
                set_context(
                    effect,
                    D2D1_COLORMANAGEMENT_PROP_DESTINATION_COLOR_CONTEXT.0,
                    to,
                )?;
                let quality = D2D1_COLORMANAGEMENT_PROP_QUALITY.0 as u32;
                set_enum(effect, quality, D2D1_COLORMANAGEMENT_QUALITY_BEST.0)?;
            }
            set_bool(&curve, D2D1_TABLETRANSFER_PROP_ALPHA_DISABLE.0 as u32, true)?;
            // Effects work in 8-bit buffers on an 8-bit target, which would clip everything
            // brighter or more saturated than SDR before it is mapped.
            let precision = D2D1_PROPERTY_PRECISION.0 as u32;
            for e in [&linear, &scale, &curve, &srgb] {
                set_enum(e, precision, D2D1_BUFFER_PRECISION_16BPC_FLOAT.0)?;
            }
            chain(&scale, &linear);
            chain(&curve, &scale);
            chain(&srgb, &curve);
            Ok(ToneMap {
                linear,
                scale,
                curve,
                srgb,
                peak: Cell::new(0.0),
            })
        }
    }

    /// What to draw for `hdr10`, an RGB frame still PQ-encoded in BT.2020 whose brightest light
    /// is `peak` nits.
    pub fn apply(&self, hdr10: &ID2D1Bitmap1, peak: f32) -> Result<ID2D1Image> {
        let peak = peak.max(HDR_WHITE);
        unsafe {
            if self.peak.get() != peak {
                // The effect scales by input / output white: from 80 nits per 1.0 to the peak.
                let input = D2D1_WHITELEVELADJUSTMENT_PROP_INPUT_WHITE_LEVEL.0 as u32;
                set_f32(&self.scale, input, SDR_WHITE)?;
                let output = D2D1_WHITELEVELADJUSTMENT_PROP_OUTPUT_WHITE_LEVEL.0 as u32;
                set_f32(&self.scale, output, peak)?;
                let table: Vec<u8> = (0..TABLE)
                    .flat_map(|i| curve(i as f32 / (TABLE - 1) as f32, peak).to_ne_bytes())
                    .collect();
                for channel in [
                    D2D1_TABLETRANSFER_PROP_RED_TABLE,
                    D2D1_TABLETRANSFER_PROP_GREEN_TABLE,
                    D2D1_TABLETRANSFER_PROP_BLUE_TABLE,
                ] {
                    let index = channel.0 as u32;
                    self.curve
                        .SetValue(index, D2D1_PROPERTY_TYPE_BLOB, &table)?;
                }
                self.peak.set(peak);
            }
            self.linear
                .SetInput(0, Some(&hdr10.cast::<ID2D1Image>()?), true);
            self.srgb.GetOutput()
        }
    }
}

/// The SDR level, 1.0 for SDR white, of light at `x` times the stream's `peak` nits. Light up
/// to `KNEE` of reference white keeps its level; above it, extended Reinhard (slope 1 at the knee,
/// so no visible seam) brings the peak to SDR white.
fn curve(x: f32, peak: f32) -> f32 {
    let level = x * peak / HDR_WHITE;
    let max = peak / HDR_WHITE;
    if level <= KNEE || max <= 1.0 {
        return level.min(1.0);
    }
    let u = (level - KNEE) / (1.0 - KNEE);
    let w = (max - KNEE) / (1.0 - KNEE);
    let rolled = u * (1.0 + u / (w * w)) / (1.0 + u);
    (KNEE + (1.0 - KNEE) * rolled).min(1.0)
}

/// Feeds `from`'s output into `to`, as d2d1_1helper.h's SetInputEffect does.
unsafe fn chain(to: &ID2D1Effect, from: &ID2D1Effect) {
    unsafe {
        let output = from.GetOutput().ok();
        to.SetInput(0, output.as_ref(), true);
    }
}

unsafe fn set_f32(effect: &ID2D1Effect, index: u32, value: f32) -> Result<()> {
    unsafe { effect.SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &value.to_ne_bytes()) }
}

unsafe fn set_enum(effect: &ID2D1Effect, index: u32, value: i32) -> Result<()> {
    unsafe {
        effect.SetValue(
            index,
            D2D1_PROPERTY_TYPE_ENUM,
            &(value as u32).to_ne_bytes(),
        )
    }
}

unsafe fn set_bool(effect: &ID2D1Effect, index: u32, value: bool) -> Result<()> {
    unsafe {
        effect.SetValue(
            index,
            D2D1_PROPERTY_TYPE_BOOL,
            &u32::from(value).to_ne_bytes(),
        )
    }
}

/// A colour context property takes the interface pointer itself.
unsafe fn set_context(
    effect: &ID2D1Effect,
    index: i32,
    context: &ID2D1ColorContext1,
) -> Result<()> {
    let pointer = context.as_raw() as usize;
    unsafe {
        effect.SetValue(
            index as u32,
            D2D1_PROPERTY_TYPE_COLOR_CONTEXT,
            &pointer.to_ne_bytes(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_keeps_diffuse_light_and_fits_the_peak() {
        let peak = 1000.0;
        let at = |nits: f32| curve(nits / peak, peak);
        // Below the knee, levels are kept: 101.5 nits is half of reference white.
        assert!((at(101.5) - 0.5).abs() < 1e-6);
        // The peak lands on SDR white, and the curve only ever rises.
        assert!((at(peak) - 1.0).abs() < 1e-5);
        let steps: Vec<f32> = (0..=1000).map(|i| curve(i as f32 / 1000.0, peak)).collect();
        assert!(steps.windows(2).all(|w| w[1] >= w[0]));
        // No step at the knee: just above it, the slope is still about 1.
        let knee = KNEE * HDR_WHITE;
        assert!(((at(knee + 1.0) - at(knee)) * HDR_WHITE - 1.0).abs() < 0.02);
        // A stream no brighter than reference white needs no roll-off.
        assert_eq!(curve(1.0, 203.0), 1.0);
        assert!((curve(0.5, 150.0) - 75.0 / 203.0).abs() < 1e-6);
    }
}
