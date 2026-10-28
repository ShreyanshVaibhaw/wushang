//! BT.2020 video (HDR10, HLG and SDR) brought to the back buffer with Direct2D effects. The
//! video processor only turns a frame from YCbCr into RGB, still encoded and in BT.2020, which
//! every driver that supports HDR displays can do. Direct2D does the rest:
//!
//! 1. To linear scRGB: BT.709 primaries, BT.2020 colours outside 0..1. PQ goes through a colour
//!    management effect. HLG and SDR go through a table of their transfer function and a matrix
//!    to BT.709, as Direct2D has no colour context for HLG, and video processors that do the
//!    BT.2020 to BT.709 step for SDR may skip the matrix and leave colours pale (this laptop's
//!    Intel one does). HLG also gets its system gamma (BT.2100, 1.2 for a 1000-nit display) on
//!    luminance, so it doesn't change saturation.
//! 2. Scaled so the stream's peak is 1.0, the domain of the next step's table.
//! 3. A tone curve (`curve`) per channel. On an SDR screen light is unchanged up to most of HDR's
//!    reference white (203 nits, BT.2408) and highlights above roll off into the rest of SDR's
//!    range. On an HDR screen light keeps its nits up to most of the screen's peak instead.
//! 4. For SDR, linear to sRGB. For HDR, back to scRGB's 80 nits per 1.0.
//!
//! Direct2D's own HDR Tone Map effect was tried first; its curve darkened even mid-greys by a third.
//!
//! On an HDR screen the interface is drawn in sRGB as ever, then `SdrLayer` brings it to scRGB at
//! the brightness Windows shows SDR content at.

use std::cell::Cell;

use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, Result};

use crate::hdr::Screen;

/// SDR white in scRGB, and where HDR video puts its diffuse white (BT.2408).
const SDR_WHITE: f32 = 80.0;
const HDR_WHITE: f32 = 203.0;
/// The display HLG is graded for: its signal's 1.0 is this many nits.
const HLG_PEAK: f32 = 1000.0;
/// BT.2020 luminance weights, and its linear RGB in BT.709 primaries (BT.2087).
const LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];
const BT2020_TO_709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];

/// How a BT.2020 frame is encoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal {
    /// HDR10.
    Pq,
    Hlg,
    /// SDR. Decoded with BT.1886's gamma 2.4, as BT.709 masters are converted to BT.2020, and
    /// encoded with it again after the matrix, so it shows as BT.709 video does: the same
    /// tones, only the colours change.
    Sdr,
}
/// Light up to this share of reference white passes unchanged.
const KNEE: f32 = 0.75;
/// Table entries; Direct2D interpolates between them.
const TABLE: usize = 1024;

pub struct ToneMap {
    /// PQ to linear scRGB.
    linear: ID2D1Effect,
    /// HLG or SDR to linear BT.2020, HLG then through its system gamma in `ootf` (wired up once
    /// in `new`), both through `gamut` to scRGB, SDR then through `display` to the light the
    /// screen would show for it as BT.709 video.
    decode: ID2D1Effect,
    ootf: ID2D1Effect,
    gamut: ID2D1Effect,
    display: ID2D1Effect,
    scale: ID2D1Effect,
    curve: ID2D1Effect,
    srgb: ID2D1Effect,
    /// For an HDR screen: the curve's 0..1 back to nits, in scRGB.
    boost: ID2D1Effect,
    /// The signal, stream peak and screen the effects are set for.
    set: Cell<Option<(Signal, f32, Option<Screen>)>>,
}

impl ToneMap {
    pub fn new(ctx: &ID2D1DeviceContext5) -> Result<Self> {
        unsafe {
            let linear = ctx.CreateEffect(&CLSID_D2D1ColorManagement)?;
            let decode = ctx.CreateEffect(&CLSID_D2D1TableTransfer)?;
            let luma = ctx.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            let gamma = ctx.CreateEffect(&CLSID_D2D1TableTransfer)?;
            let ootf = ctx.CreateEffect(&CLSID_D2D1ArithmeticComposite)?;
            let gamut = ctx.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            let display = ctx.CreateEffect(&CLSID_D2D1TableTransfer)?;
            let scale = ctx.CreateEffect(&CLSID_D2D1WhiteLevelAdjustment)?;
            let curve = ctx.CreateEffect(&CLSID_D2D1TableTransfer)?;
            let srgb = ctx.CreateEffect(&CLSID_D2D1ColorManagement)?;
            let boost = ctx.CreateEffect(&CLSID_D2D1WhiteLevelAdjustment)?;
            let space = |s| ctx.CreateColorContextFromDxgiColorSpace(s);
            let hdr10 = space(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)?;
            let scrgb = space(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
            let srgb_space = space(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)?;
            for (effect, from, to) in [(&linear, &hdr10, &scrgb), (&srgb, &scrgb, &srgb_space)] {
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
            for table in [&decode, &gamma, &display, &curve] {
                set_bool(table, D2D1_TABLETRANSFER_PROP_ALPHA_DISABLE.0 as u32, true)?;
            }
            // Luminance into every channel; the light times luminance to the power 0.2.
            set_matrix(&luma, [LUMA; 3])?;
            set_table(&gamma, |y| y.powf(0.2))?;
            let coefficients = D2D1_ARITHMETICCOMPOSITE_PROP_COEFFICIENTS.0 as u32;
            let product: Vec<u8> = [1f32, 0.0, 0.0, 0.0]
                .iter()
                .flat_map(|c| c.to_ne_bytes())
                .collect();
            ootf.SetValue(coefficients, D2D1_PROPERTY_TYPE_VECTOR4, &product)?;
            set_matrix(&gamut, BT2020_TO_709)?;
            set_table(&display, |l| srgb_linear(l.powf(1.0 / 2.4)))?;
            chain(&display, &gamut);
            // Effects work in 8-bit buffers on an 8-bit target, which would clip everything
            // brighter or more saturated than SDR before it is mapped.
            let precision = D2D1_PROPERTY_PRECISION.0 as u32;
            for e in [
                &linear, &decode, &luma, &gamma, &ootf, &gamut, &display, &scale, &curve, &srgb,
                &boost,
            ] {
                set_enum(e, precision, D2D1_BUFFER_PRECISION_16BPC_FLOAT.0)?;
            }
            chain(&luma, &decode);
            chain(&gamma, &luma);
            chain(&ootf, &decode);
            let output = gamma.GetOutput().ok();
            ootf.SetInput(1, output.as_ref(), true);
            chain(&curve, &scale);
            chain(&srgb, &curve);
            chain(&boost, &curve);
            Ok(ToneMap {
                linear,
                decode,
                ootf,
                gamut,
                display,
                scale,
                curve,
                srgb,
                boost,
                set: Cell::new(None),
            })
        }
    }

    /// What to draw for `frame`, RGB still encoded as `signal` in BT.2020; `peak` is a PQ
    /// stream's brightest light in nits. sRGB, or scRGB for an HDR `screen`.
    pub fn apply(
        &self,
        frame: &ID2D1Bitmap1,
        signal: Signal,
        peak: f32,
        screen: Option<Screen>,
    ) -> Result<ID2D1Image> {
        // The nits linear light's 1.0 stands for, and the stream's peak. SDR white goes where
        // the screen shows SDR content: on an SDR screen, at the curve's top.
        let sdr_white = screen.map_or(HDR_WHITE, |s| s.white);
        let (unit, peak) = match signal {
            Signal::Pq => (SDR_WHITE, peak),
            Signal::Hlg => (HLG_PEAK, HLG_PEAK),
            Signal::Sdr => (sdr_white, sdr_white),
        };
        let peak = peak.max(HDR_WHITE);
        // The light the curve's 1.0 stands for: SDR white, or the screen's peak.
        let top = screen.map_or(HDR_WHITE, |s| s.peak.max(HDR_WHITE));
        let key = Some((signal, peak, screen));
        unsafe {
            if self.set.get() != key {
                // From `unit` nits per 1.0 to the peak, then from the top to 80 nits per 1.0.
                scale_white(&self.scale, unit, peak)?;
                scale_white(&self.boost, top, SDR_WHITE)?;
                set_table(&self.curve, |x| curve(x, peak, top))?;
                let head = match signal {
                    Signal::Pq => &self.linear,
                    Signal::Hlg => {
                        set_table(&self.decode, hlg_scene)?;
                        chain(&self.gamut, &self.ootf);
                        &self.gamut
                    }
                    Signal::Sdr => {
                        set_table(&self.decode, |v| v.powf(2.4))?;
                        chain(&self.gamut, &self.decode);
                        &self.display
                    }
                };
                chain(&self.scale, head);
                self.set.set(key);
            }
            let first = match signal {
                Signal::Pq => &self.linear,
                _ => &self.decode,
            };
            first.SetInput(0, Some(&frame.cast::<ID2D1Image>()?), true);
            if screen.is_some() {
                self.boost.GetOutput()
            } else {
                self.srgb.GetOutput()
            }
        }
    }
}

/// The interface, drawn in sRGB, brought to an HDR screen's scRGB back buffer.
pub struct SdrLayer {
    linear: ID2D1Effect,
    white: ID2D1Effect,
    /// The SDR white level, in nits, the effect is set for.
    level: Cell<f32>,
}

impl SdrLayer {
    pub fn new(ctx: &ID2D1DeviceContext5) -> Result<Self> {
        unsafe {
            let linear = ctx.CreateEffect(&CLSID_D2D1ColorManagement)?;
            let white = ctx.CreateEffect(&CLSID_D2D1WhiteLevelAdjustment)?;
            let space = |s| ctx.CreateColorContextFromDxgiColorSpace(s);
            let source = D2D1_COLORMANAGEMENT_PROP_SOURCE_COLOR_CONTEXT.0;
            set_context(
                &linear,
                source,
                &space(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)?,
            )?;
            let destination = D2D1_COLORMANAGEMENT_PROP_DESTINATION_COLOR_CONTEXT.0;
            set_context(
                &linear,
                destination,
                &space(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?,
            )?;
            let precision = D2D1_PROPERTY_PRECISION.0 as u32;
            for e in [&linear, &white] {
                set_enum(e, precision, D2D1_BUFFER_PRECISION_16BPC_FLOAT.0)?;
            }
            chain(&white, &linear);
            Ok(SdrLayer {
                linear,
                white,
                level: Cell::new(0.0),
            })
        }
    }

    /// What to draw for `layer`, sRGB with alpha, so its white shows at `white` nits.
    pub fn apply(&self, layer: &ID2D1Bitmap1, white: f32) -> Result<ID2D1Image> {
        unsafe {
            if self.level.get() != white {
                scale_white(&self.white, white, SDR_WHITE)?;
                self.level.set(white);
            }
            self.linear
                .SetInput(0, Some(&layer.cast::<ID2D1Image>()?), true);
            self.white.GetOutput()
        }
    }
}

/// Sets a White Level Adjustment effect to multiply by `input / output`.
unsafe fn scale_white(effect: &ID2D1Effect, input: f32, output: f32) -> Result<()> {
    unsafe {
        let index = D2D1_WHITELEVELADJUSTMENT_PROP_INPUT_WHITE_LEVEL.0 as u32;
        set_f32(effect, index, input)?;
        let index = D2D1_WHITELEVELADJUSTMENT_PROP_OUTPUT_WHITE_LEVEL.0 as u32;
        set_f32(effect, index, output)
    }
}

/// HLG's inverse OETF (BT.2100): scene light, 0..1, of signal `e`.
fn hlg_scene(e: f32) -> f32 {
    let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / a).exp() + b) / 12.0
    }
}

/// The sRGB curve, decoding: the light a screen shows for BT.709 video's level `v`.
fn srgb_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Sets all three channel tables of a Table Transfer effect to `f` over 0..1.
unsafe fn set_table(effect: &ID2D1Effect, f: impl Fn(f32) -> f32) -> Result<()> {
    let table: Vec<u8> = (0..TABLE)
        .flat_map(|i| f(i as f32 / (TABLE - 1) as f32).to_ne_bytes())
        .collect();
    for channel in [
        D2D1_TABLETRANSFER_PROP_RED_TABLE,
        D2D1_TABLETRANSFER_PROP_GREEN_TABLE,
        D2D1_TABLETRANSFER_PROP_BLUE_TABLE,
    ] {
        unsafe { effect.SetValue(channel.0 as u32, D2D1_PROPERTY_TYPE_BLOB, &table)? };
    }
    Ok(())
}

/// Sets a Color Matrix effect to give each output channel `rgb[out][in]` of the inputs and keep
/// alpha.
unsafe fn set_matrix(effect: &ID2D1Effect, rgb: [[f32; 3]; 3]) -> Result<()> {
    // Direct2D's 5x4 matrix has a row per input (R, G, B, A, offset) and a column per output.
    let mut m = [0f32; 20];
    for (out, row) in rgb.iter().enumerate() {
        for (input, &v) in row.iter().enumerate() {
            m[input * 4 + out] = v;
        }
    }
    m[15] = 1.0;
    let bytes: Vec<u8> = m.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let index = D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32;
    unsafe { effect.SetValue(index, D2D1_PROPERTY_TYPE_MATRIX_5X4, &bytes) }
}

/// The level, 1.0 for `top` nits, of light at `x` times the stream's `peak` nits. Light up to
/// `KNEE` of the top keeps its level; above it, extended Reinhard (slope 1 at the knee, so no
/// visible seam) brings the peak to the top.
fn curve(x: f32, peak: f32, top: f32) -> f32 {
    let level = x * peak / top;
    let max = peak / top;
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
        let at = |nits: f32| curve(nits / peak, peak, HDR_WHITE);
        // Below the knee, levels are kept: 101.5 nits is half of reference white.
        assert!((at(101.5) - 0.5).abs() < 1e-6);
        // The peak lands on SDR white, and the curve only ever rises.
        assert!((at(peak) - 1.0).abs() < 1e-5);
        let steps: Vec<f32> = (0..=1000)
            .map(|i| curve(i as f32 / 1000.0, peak, HDR_WHITE))
            .collect();
        assert!(steps.windows(2).all(|w| w[1] >= w[0]));
        // No step at the knee: just above it, the slope is still about 1.
        let knee = KNEE * HDR_WHITE;
        assert!(((at(knee + 1.0) - at(knee)) * HDR_WHITE - 1.0).abs() < 0.02);
        // A stream no brighter than reference white needs no roll-off.
        assert_eq!(curve(1.0, 203.0, HDR_WHITE), 1.0);
        assert!((curve(0.5, 150.0, HDR_WHITE) - 75.0 / 203.0).abs() < 1e-6);
        // A 600-nit screen keeps 300 nits as they are and fits a 4000-nit peak.
        assert!((curve(300.0 / 4000.0, 4000.0, 600.0) - 0.5).abs() < 1e-6);
        assert!((curve(1.0, 4000.0, 600.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn hdr_screens_get_nits_and_the_interface_at_sdr_white() {
        let ctx = warp();
        let pq = |nits: f32| {
            let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.6875);
            let y = (nits / 10000.0).powf(m1);
            ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
        };
        // A 100-nit grey, PQ-encoded in a 10-bit texture as the video processor leaves it.
        let v = (pq(100.0) * 1023.0).round() as u32;
        let grey = v | v << 10 | v << 20 | 3 << 30;
        let frame = source(&ctx, DXGI_FORMAT_R10G10B10A2_UNORM, &grey.to_ne_bytes());
        let tonemap = ToneMap::new(&ctx).unwrap();
        // On a 1000-nit screen it keeps its nits: 1.25 in scRGB's 80 nits per 1.0.
        let screen = Some(Screen {
            peak: 1000.0,
            white: 203.0,
        });
        let hdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Pq, 1000.0, screen).unwrap(),
        );
        assert!((hdr[0] - 1.25).abs() < 0.03, "{hdr:?}");
        // On an SDR screen it is half of reference white, sRGB-encoded.
        let sdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Pq, 1000.0, None).unwrap(),
        );
        assert!((sdr[0] - 0.726).abs() < 0.01, "{sdr:?}");

        // Interface white at a 240-nit SDR white level is 3.0, and mid grey 0.2158 * 3.
        let ui = source(
            &ctx,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            &[255, 255, 255, 255, 128, 128, 128, 255],
        );
        let layer = SdrLayer::new(&ctx).unwrap();
        let lit = draw(&ctx, &layer.apply(&ui, 240.0).unwrap());
        assert!((lit[0] - 3.0).abs() < 0.03, "{lit:?}");
        assert!((lit[4] - 0.647).abs() < 0.02, "{lit:?}");
    }

    #[test]
    fn sdr_bt2020_keeps_its_saturation_and_tones() {
        let ctx = warp();
        let tonemap = ToneMap::new(&ctx).unwrap();
        // BT.709 red in BT.2020, then a mid grey, gamma 2.4-encoded in 10 bits.
        let frame = source(
            &ctx,
            DXGI_FORMAT_R10G10B10A2_UNORM,
            &rgb10(&[[842, 336, 185], [512; 3]]),
        );
        // Red again on an SDR screen, not the pale (0.82, 0.33, 0.18) of skipping the matrix,
        // and the grey as it was.
        let sdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Sdr, 0.0, None).unwrap(),
        );
        assert!(
            (sdr[0] - 1.0).abs() < 0.01 && sdr[1] < 0.03 && sdr[2] < 0.03,
            "{sdr:?}"
        );
        assert!((sdr[4] - 0.5).abs() < 0.01, "{sdr:?}");
        // On an HDR screen the grey goes where Windows puts SDR content: 0.2145 of 240 nits.
        let screen = Some(Screen {
            peak: 1000.0,
            white: 240.0,
        });
        let hdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Sdr, 0.0, screen).unwrap(),
        );
        assert!((hdr[4] - 0.643).abs() < 0.01, "{hdr:?}");
    }

    #[test]
    fn hlg_puts_reference_white_at_203_nits() {
        let ctx = warp();
        let tonemap = ToneMap::new(&ctx).unwrap();
        // A grey and a BT.2020 green, both at 75% signal, HLG's reference white.
        let frame = source(
            &ctx,
            DXGI_FORMAT_R10G10B10A2_UNORM,
            &rgb10(&[[767; 3], [0, 767, 0]]),
        );
        let screen = Some(Screen {
            peak: 1000.0,
            white: 203.0,
        });
        let hdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Hlg, 0.0, screen).unwrap(),
        );
        // 203 nits in scRGB. The green's system gamma goes by its luminance, not its channel:
        // 2.66 in BT.709, where a per-channel gamma would give 2.87.
        assert!((hdr[0] - 2.536).abs() < 0.03, "{hdr:?}");
        assert!((hdr[5] - 2.658).abs() < 0.03 && hdr[4] < 0.01, "{hdr:?}");
        // On an SDR screen reference white rolls off with the highlights, as HDR10's does.
        let sdr = draw(
            &ctx,
            &tonemap.apply(&frame, Signal::Hlg, 0.0, None).unwrap(),
        );
        assert!((sdr[0] - 0.943).abs() < 0.01, "{sdr:?}");
    }

    /// Pixels of 10-bit RGB, packed as R10G10B10A2 with opaque alpha.
    fn rgb10(pixels: &[[u32; 3]]) -> Vec<u8> {
        pixels
            .iter()
            .flat_map(|[r, g, b]| (r | g << 10 | b << 20 | 3 << 30).to_ne_bytes())
            .collect()
    }

    fn warp() -> ID2D1DeviceContext5 {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::IDXGIDevice;
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
            let dxgi: IDXGIDevice = device.unwrap().cast().unwrap();
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            factory
                .CreateDevice(&dxgi)
                .unwrap()
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .unwrap()
                .cast()
                .unwrap()
        }
    }

    fn props(format: DXGI_FORMAT, options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
        D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: Common::D2D1_PIXEL_FORMAT {
                format,
                alphaMode: Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: options,
            ..Default::default()
        }
    }

    /// A bitmap one row of 4-byte `pixels` wide.
    fn source(ctx: &ID2D1DeviceContext5, format: DXGI_FORMAT, pixels: &[u8]) -> ID2D1Bitmap1 {
        let size = Common::D2D_SIZE_U {
            width: pixels.len() as u32 / 4,
            height: 1,
        };
        let props = props(format, D2D1_BITMAP_OPTIONS_NONE);
        unsafe {
            ctx.CreateBitmap(
                size,
                Some(pixels.as_ptr().cast()),
                pixels.len() as u32,
                &props,
            )
            .unwrap()
        }
    }

    /// Draws `image` into a 16-bit float target; returns its first two pixels' RGBA.
    fn draw(ctx: &ID2D1DeviceContext5, image: &ID2D1Image) -> [f32; 8] {
        let size = Common::D2D_SIZE_U {
            width: 2,
            height: 1,
        };
        let float = DXGI_FORMAT_R16G16B16A16_FLOAT;
        unsafe {
            let target = ctx
                .CreateBitmap(size, None, 0, &props(float, D2D1_BITMAP_OPTIONS_TARGET))
                .unwrap();
            ctx.SetTarget(&target);
            ctx.BeginDraw();
            ctx.Clear(None);
            ctx.DrawImage(
                image,
                None,
                None,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                Common::D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
            ctx.EndDraw(None, None).unwrap();
            ctx.SetTarget(None);
            let options = D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW;
            let read = ctx
                .CreateBitmap(size, None, 0, &props(float, options))
                .unwrap();
            read.CopyFromBitmap(None, &target, None).unwrap();
            let mapped = read.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let halves = std::slice::from_raw_parts(mapped.bits as *const u16, 8);
            let out = std::array::from_fn(|i| half(halves[i]));
            read.Unmap().unwrap();
            out
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
}
