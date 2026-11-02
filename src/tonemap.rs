//! BT.2020 video (HDR10, HLG and SDR) brought to the screen. The Metal shader (shaders/video.metal)
//! does this per pixel after YCbCr to RGB; this file holds the numbers it is given and the same
//! math on the CPU, which the tests hold the shader to.
//!
//! 1. To linear light. PQ through its EOTF; HLG through its inverse OETF and its system gamma
//!    (BT.2100, 1.2 for a 1000-nit display) on luminance, so it doesn't change saturation; SDR
//!    through BT.1886's gamma 2.4.
//! 2. A matrix to the output's primaries: BT.709 for an sRGB layer, Display P3 for an EDR one.
//!    SDR BT.2020 is encoded with gamma 2.4 again after it, so it shows as BT.709 video does: the
//!    same tones, only the colours change.
//! 3. Scaled so the stream's peak is 1.0, then a tone curve (`curve`) per channel. On an SDR
//!    screen light is unchanged up to most of HDR's reference white (203 nits, BT.2408) and
//!    highlights above roll off into the rest of SDR's range. On an EDR screen light keeps its
//!    nits up to most of the screen's headroom instead.
//! 4. For SDR, linear to sRGB. For EDR, to the layer's linear values, where 1.0 is SDR white.
//!
//! The interface stays at SDR white on an EDR screen: the shader linearises it into the same
//! extended Display P3 space as the video.

use crate::hdr::Screen;

/// Where HDR video puts its diffuse white (BT.2408), and SDR white when a screen won't say.
pub const HDR_WHITE: f32 = 203.0;
/// The display HLG is graded for: its signal's 1.0 is this many nits.
const HLG_PEAK: f32 = 1000.0;
/// PQ's 1.0.
const PQ_PEAK: f32 = 10000.0;
/// BT.2020 luminance weights.
#[cfg(test)]
const LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// Linear BT.2020 in BT.709 primaries (BT.2087), in Display P3 primaries, and BT.709 in P3.
pub const BT2020_TO_709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];
pub const BT2020_TO_P3: [[f32; 3]; 3] = [
    [1.343_578, -0.282_180, -0.061_399],
    [-0.065_297, 1.075_788, -0.010_490],
    [0.002_822, -0.019_598, 1.016_777],
];
pub const BT709_TO_P3: [[f32; 3]; 3] = [
    [0.822_462, 0.177_538, 0.0],
    [0.033_194, 0.966_806, 0.0],
    [0.017_083, 0.072_397, 0.910_520],
];

/// How a BT.2020 frame is encoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal {
    /// HDR10.
    Pq,
    Hlg,
    /// SDR. Decoded with BT.1886's gamma 2.4, as BT.709 masters are converted to BT.2020, and
    /// encoded with it again after the matrix.
    Sdr,
}

impl Signal {
    /// Whether the signal holds light above SDR white, which an EDR screen can show.
    pub fn is_hdr(self) -> bool {
        matches!(self, Signal::Pq | Signal::Hlg)
    }

    /// The shader's number for it.
    pub fn code(self) -> i32 {
        match self {
            Signal::Pq => 1,
            Signal::Hlg => 2,
            Signal::Sdr => 3,
        }
    }
}

/// Light up to this share of the curve's top passes unchanged.
#[cfg(test)]
const KNEE: f32 = 0.75;

/// The shader's tone numbers for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// Nits of the decoded light's 1.0.
    pub nits: f32,
    /// The stream's brightest light, in nits.
    pub peak: f32,
    /// The light the curve's 1.0 stands for: SDR white, or the screen's peak.
    pub top: f32,
    /// What the curve's 1.0 becomes in an EDR layer, where 1.0 is SDR white.
    pub scale: f32,
}

/// The tone numbers for `signal`; `peak` is a PQ stream's brightest light in nits, `screen` the
/// EDR screen it goes to, or None for SDR.
pub fn tone(signal: Signal, peak: f32, screen: Option<Screen>) -> Tone {
    // SDR white goes where the screen shows SDR content: on an SDR screen, at the curve's top.
    let sdr_white = screen.map_or(HDR_WHITE, |s| s.white);
    let (nits, peak) = match signal {
        Signal::Pq => (PQ_PEAK, peak),
        Signal::Hlg => (HLG_PEAK, HLG_PEAK),
        Signal::Sdr => (sdr_white, sdr_white),
    };
    let top = screen.map_or(HDR_WHITE, |s| s.peak.max(HDR_WHITE));
    Tone {
        nits,
        peak: peak.max(HDR_WHITE),
        top,
        scale: screen.map_or(1.0, |s| top / s.white),
    }
}

/// The level, 1.0 for `top` nits, of light at `x` times the stream's `peak` nits. Light up to
/// `KNEE` of the top keeps its level; above it, extended Reinhard (slope 1 at the knee, so no
/// visible seam) brings the peak to the top. The shader has the same.
#[cfg(test)]
pub fn curve(x: f32, peak: f32, top: f32) -> f32 {
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

/// HLG's inverse OETF (BT.2100): scene light, 0..1, of signal `e`.
#[cfg(test)]
pub fn hlg_scene(e: f32) -> f32 {
    let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / a).exp() + b) / 12.0
    }
}

/// SMPTE ST 2084's EOTF: light, 1.0 for 10000 nits, of signal `e`, and its inverse.
#[cfg(test)]
pub fn pq_light(e: f32) -> f32 {
    let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.6875);
    let p = e.powf(1.0 / m2);
    ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

#[cfg(test)]
pub fn pq_signal(nits: f32) -> f32 {
    let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.6875);
    let y = (nits / PQ_PEAK).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

/// The sRGB curve, decoding: the light a screen shows for BT.709 video's level `v`.
#[cfg(test)]
pub fn srgb_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB curve, encoding.
#[cfg(test)]
pub fn srgb_encode(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The shader's BT.2020 path on the CPU: R'G'B' still encoded as `signal` to the output, sRGB
/// for an SDR screen or EDR values (1.0 is SDR white) for `screen`.
#[cfg(test)]
pub fn reference(signal: Signal, rgb: [f32; 3], peak: f32, screen: Option<Screen>) -> [f32; 3] {
    let t = tone(signal, peak, screen);
    let rgb = rgb.map(|v| v.clamp(0.0, 1.0));
    let light = match signal {
        Signal::Pq => rgb.map(pq_light),
        Signal::Hlg => {
            let scene = rgb.map(hlg_scene);
            let y: f32 = scene.iter().zip(LUMA).map(|(s, w)| s * w).sum();
            scene.map(|s| s * y.clamp(0.0, 1.0).powf(0.2))
        }
        Signal::Sdr => rgb.map(|v| v.powf(2.4)),
    };
    let gamut = if screen.is_some() {
        BT2020_TO_P3
    } else {
        BT2020_TO_709
    };
    let mut light = gamut.map(|row| row.iter().zip(light).map(|(m, l)| m * l).sum::<f32>());
    if signal == Signal::Sdr {
        light = light.map(|l| srgb_linear(l.clamp(0.0, 1.0).powf(1.0 / 2.4)));
    }
    light.map(|l| {
        let level = curve((l * t.nits / t.peak).clamp(0.0, 1.0), t.peak, t.top);
        match screen {
            Some(_) => level * t.scale,
            None => srgb_encode(level),
        }
    })
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

    /// The numbers the Windows effect chains were checked against, now through `reference`.
    #[test]
    fn edr_screens_get_nits_and_sdr_screens_reference_white() {
        let grey = |nits: f32| [pq_signal(nits); 3];
        // 100 nits on a screen with 1000 nits of headroom keep their nits: 100 / 203 of SDR white.
        let screen = Some(Screen {
            peak: 1000.0,
            white: 203.0,
        });
        let edr = reference(Signal::Pq, grey(100.0), 1000.0, screen);
        assert!((edr[0] - 100.0 / 203.0).abs() < 1e-3, "{edr:?}");
        // On an SDR screen it is half of reference white, sRGB-encoded.
        let sdr = reference(Signal::Pq, grey(100.0), 1000.0, None);
        assert!((sdr[0] - 0.726).abs() < 0.01, "{sdr:?}");
        // HLG's reference white, 75% signal, is 203 nits, SDR white on an EDR screen.
        let hlg = reference(Signal::Hlg, [0.75; 3], 0.0, screen);
        assert!((hlg[0] - 1.0).abs() < 0.01, "{hlg:?}");
        let hlg = reference(Signal::Hlg, [0.75; 3], 0.0, None);
        assert!((hlg[0] - 0.943).abs() < 0.01, "{hlg:?}");
        // BT.709 red in BT.2020 comes back red on an SDR screen, and a mid grey as it was.
        let red = reference(
            Signal::Sdr,
            [842.0 / 1023.0, 336.0 / 1023.0, 185.0 / 1023.0],
            0.0,
            None,
        );
        assert!(
            (red[0] - 1.0).abs() < 0.01 && red[1] < 0.03 && red[2] < 0.03,
            "{red:?}"
        );
        let mid = reference(Signal::Sdr, [512.0 / 1023.0; 3], 0.0, None);
        assert!((mid[0] - 0.5).abs() < 0.01, "{mid:?}");
    }

    #[test]
    fn edr_headroom_sets_the_top() {
        // Four times SDR white: 812 nits, of which three quarters keep their level.
        let screen = Screen {
            peak: 4.0 * HDR_WHITE,
            white: HDR_WHITE,
        };
        let t = tone(Signal::Pq, 4000.0, Some(screen));
        assert_eq!(
            (t.nits, t.peak, t.top, t.scale),
            (10000.0, 4000.0, 812.0, 4.0)
        );
        // The interface's white and SDR BT.2020 stay at SDR white.
        let t = tone(Signal::Sdr, 0.0, Some(screen));
        assert!((t.nits / t.top * t.scale - 1.0).abs() < 1e-6);
        // Without headroom yet the curve is SDR's.
        let t = tone(
            Signal::Hlg,
            0.0,
            Some(Screen {
                peak: 203.0,
                white: 203.0,
            }),
        );
        assert_eq!((t.top, t.scale), (203.0, 1.0));
    }
}
