//! The light theme and the accent colour. The interface is written in the design's dark colours;
//! every brush and icon colour goes through [`map`] when it's created, which turns them into the
//! active theme's. A theme change rebuilds the painter's resources, so nothing else has to know.
//!
//! The design has one light frame ("Player - Light"), and its greys are the dark ones turned
//! upside down: the bars, lighter than the list in the dark theme, are darker than it in light,
//! and bright text turns dark. So light is one falling curve fitted to that frame, and lines and
//! raised boxes keep their contrast without being told apart.
//!
//! Every coloured constant is a shade of the design's orange, so the accent replaces it by the
//! same ratio; the dark tints behind selected things become pale tints on light.

use std::cell::Cell;

/// The design's orange.
pub const DEFAULT_ACCENT: u32 = 0xD37918;
/// The accent colours, in the design's order. Each is sampled where it's picked and so drawn
/// bright (the "Onboarding" frames); the design dims the ones not picked.
pub const ACCENTS: [u32; 9] = [
    0xD37918, 0xEA4C31, 0xC4A317, 0x1BA035, 0x17ACCD, 0x175FCD, 0x8E43F8, 0xEA55B3, 0xAFAFAF,
];

const FIXED: u32 = 1 << 24;

/// A colour no theme changes: things drawn over the video, colour swatches.
pub const fn fixed(rgb: u32) -> u32 {
    rgb | FIXED
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub light: bool,
    pub accent: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            light: false,
            accent: DEFAULT_ACCENT,
        }
    }
}

thread_local! {
    static THEME: Cell<Theme> = Cell::new(Theme::default());
}

pub fn current() -> Theme {
    THEME.with(Cell::get)
}

pub fn set(theme: Theme) {
    THEME.with(|t| t.set(theme));
}

/// Whether Windows apps are set to light mode (Settings > Personalization > Colors).
pub fn system_light() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut value = 0u32;
    let mut size = 4u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    read.is_ok() && value != 0
}

/// A design colour, possibly marked [`fixed`], in the active theme.
pub fn map(c: u32) -> u32 {
    map_in(current(), c)
}

fn map_in(t: Theme, c: u32) -> u32 {
    let rgb = c & 0xFF_FFFF;
    if c & FIXED != 0 {
        return rgb;
    }
    let [_, r, g, b] = rgb.to_be_bytes();
    if r == g && g == b {
        return match (t.light, r) {
            (false, _) => rgb,
            // Black is shadows, veils and the video's surround.
            (true, 0) => 0,
            (true, v) => grey(light_grey(v)),
        };
    }
    // A shade of the design's orange: k = 1 is the accent itself.
    let k = f32::from(r.max(g).max(b)) / f32::from(0xD3u8);
    if !t.light {
        return if t.accent == DEFAULT_ACCENT {
            rgb
        } else {
            scale(t.accent, k)
        };
    }
    if k < 0.6 {
        // A dark tint behind a selection: on light, a pale one over the seek track's grey.
        mix(grey(light_grey(0x09)), t.accent, (k * 1.35).min(1.0))
    } else {
        scale(t.accent, k)
    }
}

/// A dark-theme grey turned light: the brighter it was, the darker it gets.
fn light_grey(v: u8) -> u8 {
    // (dark, light) from the light frame: seek track, list, bars, a faint icon, the breadcrumb.
    const CURVE: [(f32, f32); 5] = [
        (9.0, 222.0),
        (14.0, 193.0),
        (27.0, 171.0),
        (64.0, 140.0),
        (150.0, 40.0),
    ];
    let v = f32::from(v);
    let i = CURVE
        .windows(2)
        .position(|w| v <= w[1].0)
        .unwrap_or(CURVE.len() - 2);
    let ((x0, y0), (x1, y1)) = (CURVE[i], CURVE[i + 1]);
    (y0 + (y1 - y0) * (v - x0) / (x1 - x0))
        .clamp(16.0, 232.0)
        .round() as u8
}

fn grey(v: u8) -> u32 {
    u32::from_be_bytes([0, v, v, v])
}

fn scale(rgb: u32, k: f32) -> u32 {
    let [_, r, g, b] = rgb.to_be_bytes();
    let s = |c: u8| (f32::from(c) * k).round().clamp(0.0, 255.0) as u8;
    u32::from_be_bytes([0, s(r), s(g), s(b)])
}

fn mix(a: u32, b: u32, f: f32) -> u32 {
    let ([_, ar, ag, ab], [_, br, bg, bb]) = (a.to_be_bytes(), b.to_be_bytes());
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * f).round() as u8;
    u32::from_be_bytes([0, m(ar, br), m(ag, bg), m(ab, bb)])
}

/// Rewrites every `#RRGGBB` in an icon's SVG text into the active theme.
pub fn svg(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('#') {
        out.push_str(&rest[..i]);
        let hex = rest.get(i + 1..i + 7);
        match hex.and_then(|h| u32::from_str_radix(h, 16).ok()) {
            Some(rgb) if hex.is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit())) => {
                _ = std::fmt::Write::write_fmt(&mut out, format_args!("#{:06X}", map(rgb)));
                rest = &rest[i + 7..];
            }
            _ => {
                out.push('#');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: Theme = Theme {
        light: true,
        accent: DEFAULT_ACCENT,
    };

    #[test]
    fn dark_with_the_default_accent_changes_nothing() {
        let t = Theme::default();
        for c in [0x090909, 0x8F8F8F, 0xD37918, 0x371F06] {
            assert_eq!(map_in(t, c), c & 0xFF_FFFF);
        }
    }

    #[test]
    fn light_matches_the_light_frame() {
        assert_eq!(map_in(LIGHT, 0x1B1B1B), 0xABABAB, "bars");
        assert_eq!(map_in(LIGHT, 0x0E0E0E), 0xC1C1C1, "list");
        assert_eq!(map_in(LIGHT, 0x090909), 0xDEDEDE, "seek track");
        assert_eq!(map_in(LIGHT, 0x969696), 0x282828, "breadcrumb");
        assert_eq!(map_in(LIGHT, 0xD37918), 0xD37918, "accent stays");
        assert_eq!(map_in(LIGHT, 0x000000), 0, "shadows stay black");
        // The played part of the seek bar is a pale orange, as the frame's #D7B99F.
        let [_, r, g, b] = map_in(LIGHT, 0x371F06).to_be_bytes();
        assert!(r > 0xD0 && (0xA8..0xC8).contains(&g) && (0x80..0xA8).contains(&b));
        // A line, a touch lighter than the list in the dark theme, is a touch darker in light.
        assert!(map_in(LIGHT, 0x1D1D1D) & 0xFF < 0xC1);
        assert_eq!(map_in(LIGHT, fixed(0x8F8F8F)), 0x8F8F8F);
    }

    #[test]
    fn the_accent_replaces_every_orange_by_ratio() {
        let blue = Theme {
            light: false,
            accent: 0x175FCD,
        };
        assert_eq!(map_in(blue, 0xD37918), 0x175FCD);
        assert_eq!(map_in(blue, 0x8F8F8F), 0x8F8F8F, "greys aren't accents");
        let thumb = map_in(blue, 0xB6640E).to_be_bytes();
        assert!(
            thumb[3] < 0xCD && thumb[3] > 0xA0,
            "a darker blue: {thumb:?}"
        );
    }

    #[test]
    fn icons_are_recoloured() {
        set(LIGHT);
        let text = svg(r##"<path stroke="#8F8F8F" fill="#D37918"/><a href="#x">"##);
        set(Theme::default());
        assert_eq!(
            text,
            r##"<path stroke="#303030" fill="#D37918"/><a href="#x">"##
        );
    }
}
